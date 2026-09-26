//! Main rendering pipeline: patch → MiniJinja → resolve_listing → recover → fix → serialize.

use docxtpl_compat::{patch_xml, resolve_listing_limited};
use docxtpl_xml::{ns_uri, Recovery, XmlDocument, XmlLimits};
use minijinja::value::{from_args, Kwargs, Object, ObjectRepr, Rest, ValueKind};
use minijinja::{
    escape_formatter, AutoEscape, Environment, Error, ErrorKind, State, UndefinedBehavior, Value,
};
use regex::Regex;
use serde_json::Value as JsonValue;
use std::collections::hash_map::RandomState;
use std::collections::{BTreeSet, VecDeque};
use std::fmt;
use std::hash::{BuildHasher, Hash, Hasher};
use std::io::{self, Write};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::context::{ImageRegistry, ImageResolveError, NullRegistry, RenderContext, RenderValue};
use crate::error::{RenderError, TemplateErrorKind};
use crate::fix_tables::{fix_docpr_ids, fix_tables};
use docxtpl_rich::InlineImage;

/// Default budget for a single rendered XML/controlled intermediate output.
pub(crate) const MAX_RENDERED_XML_BYTES: usize = 600 * 1024 * 1024;
/// Intermediate sequence entries created by compatibility helpers.  Each
/// entry can fan out into an owned MiniJinja value in addition to its source
/// string, so reserve a conservative 128-byte bookkeeping budget per item.
const MAX_TEMPLATE_INTERMEDIATE_ITEMS: usize = MAX_RENDERED_XML_BYTES / 128;
const MAX_TEMPLATE_FUEL: u64 = 10_000_000;

fn rendered_xml_limit(part: &str) -> RenderError {
    rendered_xml_limit_with_max(part, MAX_RENDERED_XML_BYTES)
}

fn rendered_xml_limit_with_max(part: &str, max: usize) -> RenderError {
    RenderError::Limit {
        part: part.to_string(),
        kind: "rendered_xml_bytes",
        max: max as u64,
    }
}

struct LimitedOutput {
    bytes: Vec<u8>,
    max: usize,
    exceeded: bool,
}

impl Write for LimitedOutput {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() > self.max.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(io::Error::other("rendered XML size limit exceeded"));
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Main document part name; stories, core properties, and footnotes use their
/// own part names.
pub const MAIN_PART: &str = "word/document.xml";

/// Reusable, thread-safe MiniJinja environment configurator.
///
/// The configurator runs after the docxtpl-compatible built-ins are
/// registered, so it may add or explicitly override Rust-native filters,
/// tests, and functions. It is used for the body, header/footer, footnotes,
/// and core properties of the same high-level render.
pub type EnvironmentConfigurator = Arc<dyn Fn(&mut Environment<'static>) + Send + Sync + 'static>;

/// Rendering options (code spec §3.3: options affecting output must be passed
/// explicitly; no environment variables are read).
#[derive(Clone)]
pub struct RenderOptions {
    /// Whether to auto-escape. Upstream `DocxTemplate.render(autoescape=False)` defaults to false.
    autoescape: bool,
    /// Rust-native environment extension; no attempt to host Python Jinja2 extension objects.
    environment_configurator: Option<EnvironmentConfigurator>,
    /// Budget for a single rendered XML output.
    max_rendered_xml_bytes: usize,
    /// Budget for a single MiniJinja template evaluation.
    template_fuel: u64,
}

impl RenderOptions {
    /// Compatibility mode matching upstream defaults: `autoescape=false`,
    /// lenient undefined.
    #[must_use]
    pub fn compat() -> Self {
        Self {
            autoescape: false,
            environment_configurator: None,
            max_rendered_xml_bytes: MAX_RENDERED_XML_BYTES,
            template_fuel: MAX_TEMPLATE_FUEL,
        }
    }

    /// Sets autoescape explicitly; the default matches upstream at `false` (ADR-003).
    #[must_use]
    pub fn with_autoescape(mut self, enabled: bool) -> Self {
        self.autoescape = enabled;
        self
    }

    /// Current autoescape setting.
    #[must_use]
    pub fn autoescape(&self) -> bool {
        self.autoescape
    }

    /// Sets the maximum byte count for a single rendered XML/output buffer.
    #[must_use]
    pub fn with_max_rendered_xml_bytes(mut self, max: usize) -> Self {
        self.max_rendered_xml_bytes = max;
        self
    }

    /// Current budget for a single rendered XML output.
    #[must_use]
    pub fn max_rendered_xml_bytes(&self) -> usize {
        self.max_rendered_xml_bytes
    }

    /// Sets the MiniJinja fuel for each template evaluation.
    #[must_use]
    pub fn with_template_fuel(mut self, fuel: u64) -> Self {
        self.template_fuel = fuel;
        self
    }

    /// Current template-evaluation fuel.
    #[must_use]
    pub fn template_fuel(&self) -> u64 {
        self.template_fuel
    }

    /// Adds a Rust-native MiniJinja environment configurator.
    ///
    /// Multiple calls compose in registration order rather than overriding
    /// previous configuration. The configurator must be safe to share across
    /// threads; the environment itself is still created fresh for each part
    /// render, so the configuration closure must not depend on mutable state
    /// across parts.
    #[must_use]
    pub fn with_environment_configurator<F>(mut self, configure: F) -> Self
    where
        F: Fn(&mut Environment<'static>) + Send + Sync + 'static,
    {
        let previous = self.environment_configurator.take();
        self.environment_configurator = Some(Arc::new(move |environment| {
            if let Some(previous) = previous.as_ref() {
                previous(environment);
            }
            configure(environment);
        }));
        self
    }

    fn apply_environment_configurator(&self, environment: &mut Environment<'static>) {
        if let Some(configure) = self.environment_configurator.as_ref() {
            configure(environment);
        }
    }
}

impl fmt::Debug for RenderOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RenderOptions")
            .field("autoescape", &self.autoescape)
            .field("max_rendered_xml_bytes", &self.max_rendered_xml_bytes)
            .field("template_fuel", &self.template_fuel)
            .field(
                "has_environment_configurator",
                &self.environment_configurator.is_some(),
            )
            .finish()
    }
}

impl PartialEq for RenderOptions {
    fn eq(&self, other: &Self) -> bool {
        self.autoescape == other.autoescape
            && self.max_rendered_xml_bytes == other.max_rendered_xml_bytes
            && self.template_fuel == other.template_fuel
            && match (
                self.environment_configurator.as_ref(),
                other.environment_configurator.as_ref(),
            ) {
                (None, None) => true,
                (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                _ => false,
            }
    }
}

impl Eq for RenderOptions {}

impl Default for RenderOptions {
    fn default() -> Self {
        Self::compat()
    }
}

/// Result of one render: the new part XML and recovery/healing diagnostics
/// (ADR-002: healing is never silent).
#[derive(Debug)]
pub struct RenderOutcome {
    /// The rendered and post-processed XML string.
    pub xml: String,
    /// All healing actions recorded during lenient (recover) parsing.
    pub recoveries: Vec<Recovery>,
}

/// Inserts a newline before `<w:p>` (the opening move of upstream
/// render_xml_part; solely for error line-number localization).
fn newline_before_p() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"<w:p([ >])").expect("invalid regex"))
}

/// Removes the inserted newlines after rendering.
fn newline_before_p_remove() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\n<w:p([ >])").expect("invalid regex"))
}

/// Strips XML tags (aligned with the upstream error context
/// `re.sub(r"<[^>]+>", "", line)`).
fn strip_xml_tags() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"<[^>]+>").expect("invalid regex"))
}

/// Renders an XML part (at this stage, i.e. word/document.xml).
///
/// The input is the raw template part text, and the output is XML that can be
/// written back; for semantics see docs/compatibility.md §2 and ADR-002/003.
pub fn render_document_xml(
    src_xml: &str,
    context: &JsonValue,
    options: &RenderOptions,
) -> Result<RenderOutcome, RenderError> {
    let ctx = RenderContext::try_from_json(context)
        .map_err(|error| invalid_context_value(MAIN_PART, error.to_string()))?;
    let mut null_registry = NullRegistry;
    render_document_xml_ctx(src_xml, &ctx, options, &mut null_registry)
}

/// Rich-content version of main-document rendering (P4, ADR-005): the context
/// may contain RichText / RichTextParagraph / Listing / InlineImage, with
/// images resolved by `registry`.
pub fn render_document_xml_ctx(
    src_xml: &str,
    context: &RenderContext,
    options: &RenderOptions,
    registry: &mut dyn ImageRegistry,
) -> Result<RenderOutcome, RenderError> {
    render_part_xml(
        src_xml,
        context,
        options,
        registry,
        MAIN_PART,
        PartKind::Document,
    )
}

/// Header/footer story part rendering (P5, ADR-006): shares the
/// patch → jinja → resolve_listing pipeline with the body, but performs
/// **no fix_tables / fix_docpr_ids**; shape_id is taken from the part's
/// original XML (part-level scope; docPr keeps local ids rather than being
/// renumbered from 1001 as in the body).
///
/// The output is a full lxml-style serialization (single-quoted XML
/// declaration), matching the on-disk bytes after upstream remaps the
/// header/footer to an XmlPart.
pub fn render_story_xml_ctx(
    src_xml: &str,
    context: &RenderContext,
    options: &RenderOptions,
    registry: &mut dyn ImageRegistry,
    part_name: &str,
) -> Result<RenderOutcome, RenderError> {
    render_part_xml(
        src_xml,
        context,
        options,
        registry,
        part_name,
        PartKind::Story,
    )
}

/// Footnotes part rendering (P5, ADR-006): after
/// patch → jinja → resolve_listing, **returns the string directly** without
/// XML parsing/re-serialization — upstream the footnotes part is a generic
/// binary Part, and `part._blob = rendered.encode()` preserves the template
/// declaration and unchanged bytes verbatim. The caller writes the returned
/// value back to the original part as UTF-8.
///
/// InlineImage in footnotes is unsupported (upstream raises AttributeError
/// when new_pic_inline is called on a generic Part; see DEV-0006); this entry
/// point uses [`NullRegistry`], so any image value raises an error.
pub fn render_footnotes_xml_ctx(
    src_xml: &str,
    context: &RenderContext,
    options: &RenderOptions,
    part_name: &str,
) -> Result<String, RenderError> {
    let mut null_registry = NullRegistry;
    // Images are not allowed in footnotes; shape_id is never consumed, so 0
    // suffices; normalize_input=false (generic Part, raw bytes pass through,
    // preserving the template declaration form).
    render_part_string(
        src_xml,
        context,
        options,
        &mut null_registry,
        part_name,
        0,
        false,
    )
}

/// Kinds of parts handled by the pipeline: determines shape_id scope and
/// post-render processing (P5, ADR-006).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PartKind {
    /// Body word/document.xml: fix_tables + fix_docpr_ids (docPr from 1001).
    Document,
    /// Header/footer: parse, heal, and serialize only; tables and docPr are untouched.
    Story,
}

/// Runs the full pipeline according to the part kind and returns the
/// serialized XML.
fn render_part_xml(
    src_xml: &str,
    context: &RenderContext,
    options: &RenderOptions,
    registry: &mut dyn ImageRegistry,
    part_name: &str,
    kind: PartKind,
) -> Result<RenderOutcome, RenderError> {
    let max = options.max_rendered_xml_bytes();
    // shape_id is scoped to a single part (upstream StoryPart.next_id takes
    // max(xpath("//@id")) + 1 over the original tree; the body is likewise
    // computed from the original document before rendering).
    let shape_id = shape_id_of(src_xml);

    let dst = render_part_string(
        src_xml, context, options, registry, part_name, shape_id, true,
    )?;

    // Lenient (recover) parsing/healing (safety limits remain enforced).
    let outcome =
        XmlDocument::parse_lenient(&dst, &XmlLimits::default()).map_err(|e| RenderError::Xml {
            part: part_name.to_string(),
            source: e,
        })?;
    let mut doc = outcome.doc;

    if kind == PartKind::Document {
        // Only the body runs fix_tables / fix_docpr_ids (upstream render()
        // invokes these two post-processors only on the body tree).
        fix_tables(&mut doc)?;
        fix_docpr_ids(&mut doc);
        Ok(RenderOutcome {
            xml: doc
                .try_serialize(max)
                .map_err(|_| rendered_xml_limit_with_max(part_name, max))?,
            recoveries: outcome.diagnostics,
        })
    } else {
        // When the header/footer is mapped to a new XmlPart it is parsed with
        // remove_blank_text (stripping newlines/indentation of injected image
        // XML), and there is no re-parenting; the redundant
        // xmlns:wp/xmlns:r declarations carried by wp:inline are preserved
        // verbatim (ADR-006).
        doc.strip_blank_text();
        Ok(RenderOutcome {
            xml: doc
                .try_serialize_story(max)
                .map_err(|_| rendered_xml_limit_with_max(part_name, max))?,
            recoveries: outcome.diagnostics,
        })
    }
}

/// The string stage of the pipeline: (body/header-footer) tree round-trip
/// normalization → patch → paragraph newlines → MiniJinja rendering →
/// restoration (newlines/literal escapes) → resolve_listing.
///
/// `normalize_input`: true for the body/header-footer (the upstream patch
/// input comes from a remove_blank_text lxml tree); false for footnotes — the
/// footnotes part is not registered as an XmlPart in python-docx's
/// PartFactory, so it is a generic binary Part where `part.blob` is the raw
/// on-disk bytes (Word's double-quoted declaration passes through jinja
/// verbatim, evidenced in P7b B5); patch consumes the raw bytes directly and
/// the rendered string is written back unchanged.
#[allow(clippy::too_many_arguments)]
fn render_part_string(
    src_xml: &str,
    context: &RenderContext,
    options: &RenderOptions,
    registry: &mut dyn ImageRegistry,
    part_name: &str,
    shape_id: i64,
    normalize_input: bool,
) -> Result<String, RenderError> {
    let max = options.max_rendered_xml_bytes();
    // 0. Body/header-footer: tree round-trip normalization (P7b B2) — the
    //    upstream patch_xml input is not the raw on-disk bytes: the body is
    //    `tostring(body)`, the header/footer is
    //    `tostring(parse_xml(part.blob))`, with entity decoding, indentation
    //    stripping, and lexical normalization.
    //    Footnotes keep the raw bytes (generic Part blob; declaration form
    //    untouched).
    let raw = if normalize_input {
        normalize_part_xml_with_limit(src_xml, part_name, max)?
    } else {
        src_xml.to_string()
    };
    // The Jinja2 lexer's tnewline=`\r\n|\r|\n` uniformly produces NEWLINE
    // (rendered as `\n`), i.e. the jinja round trip swallows every CR (P7b
    // B5: after the declaration of a real Word template footnote it is
    // `\r\n`, while upstream outputs `\n`). lxml tree output has no CR to
    // begin with, so this replacement is observable only on the footnotes path.
    let patch_source = raw.replace("\r\n", "\n").replace('\r', "\n");

    // 1. patch_xml: 13 bounded regex transformations.
    let patched = patch_xml(&patch_source);

    // 2. Insert a newline before each paragraph (for error localization only;
    //    undone after rendering).
    let prepared = newline_before_p()
        .replace_all(&patched, "\n<w:p${1}")
        .into_owned();

    // 3. MiniJinja rendering (defaults aligned with jinja2: lenient
    //    undefined, autoescape=false).
    //    Images first participate in rendering as placeholders (aligned with
    //    the lazy upstream InlineImage.__str__: only images actually
    //    referenced by this part's template are resolved, ADR-006); after
    //    rendering they are placed in order of appearance.
    let env = build_jinja_env_with_options(options);
    let (root, pending_images) = context_to_minijinja(context, part_name)?;
    let rendered = render_inline_value_with_limit(&env, &prepared, root, part_name, max)?;

    // 4. Undo the newlines + restore {_{ }_} literal escapes.
    let dst = newline_before_p_remove()
        .replace_all(&rendered, "<w:p${1}")
        .into_owned();
    let dst = dst
        .replace("{_{", "{{")
        .replace("}_}", "}}")
        // Upstream template.py escapes block tags by inserting "_" before
        // "{%" → "{_%" (at the patch stage "{% "→"{_%"); this prefix was
        // once written reversed as "{%_", so literal {_%- text in real
        // templates would leak into the output (P7b merge_paragraph).
        .replace("{_%", "{%")
        .replace("%_}", "%}");

    // 5. resolve_listing: \n \t \a \f → br/tab/paragraph break/page break.
    let dst = resolve_listing_limited(&dst, max)
        .map_err(|_| rendered_xml_limit_with_max(part_name, max))?;

    // 5.5 Image placeholder substitution: resolve in order of appearance in
    //     the output (rIds are deduplicated and reused by the registry per
    //     reltype/target/mode). shape_id is identical for all images of the
    //     same part: upstream StoryPart.next_id takes max(//@id)+1 over the
    //     **original part tree untouched by rendering** each time (no cache;
    //     rendering only produces strings and does not write part elements
    //     back), so multiple images get the same id/name; the body then
    //     reorders ids from 1001 via fix_docpr_ids (the name stays "Picture
    //     N"), while header/footer keeps them verbatim.
    let dst =
        substitute_images_with_limit(&dst, registry, &pending_images, shape_id, part_name, max)?;
    if dst.len() > max {
        return Err(rendered_xml_limit_with_max(part_name, max));
    }
    Ok(dst)
}

/// Normalizes an XML part to the python-docx oxml tree form (P7b B1/B2).
///
/// The two call sites share the same form:
/// 1. Before patch_xml in the rendering pipeline (B2) — the XML fed to
///    `patch_xml` for the body/header-footer comes from a python-docx oxml
///    parse tree (`remove_blank_text=True`), not from the package's raw
///    on-disk bytes: the body is `tostring(body)`, the header/footer is
///    `tostring(parse_xml(part.blob))`;
/// 2. Unconditional re-serialization of known XmlParts
///    (styles/settings/numbering) during save (B1) — when python-docx saves,
///    these parts are always rewritten from the lxml tree.
///
/// The footnotes part does not go through this function: it is not
/// registered as an XmlPart in PartFactory, so it is a generic binary Part
/// whose `blob` is the raw on-disk bytes (B5 evidence: expected keeps Word's
/// double-quoted declaration).
///
/// The tree round trip decodes entity references into literal characters in
/// text nodes (so `&quot;`/`&apos;` inside jinja expressions no longer
/// linger), strips inter-element indentation whitespace, and normalizes
/// attribute and empty-element lexical forms, emitting an lxml-style
/// single-quoted XML declaration + `\n`. A template part must be well-formed
/// XML; parse failures are reported as XML errors (matching upstream failing
/// as soon as the document is opened).
pub fn normalize_part_xml(src_xml: &str, part_name: &str) -> Result<String, RenderError> {
    normalize_part_xml_with_limit(src_xml, part_name, MAX_RENDERED_XML_BYTES)
}

fn normalize_part_xml_with_limit(
    src_xml: &str,
    part_name: &str,
    max: usize,
) -> Result<String, RenderError> {
    let mut doc = XmlDocument::parse_strict(src_xml, &XmlLimits::default()).map_err(|source| {
        RenderError::Xml {
            part: part_name.to_string(),
            source,
        }
    })?;
    doc.strip_blank_text();
    const DECLARATION: &str = "<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n";
    let subtree = doc
        .try_serialize_subtree(doc.root(), max.saturating_sub(DECLARATION.len()))
        .map_err(|_| rendered_xml_limit_with_max(part_name, max))?;
    let mut normalized = String::with_capacity(DECLARATION.len() + subtree.len());
    normalized.push_str(DECLARATION);
    normalized.push_str(&subtree);
    Ok(normalized)
}

/// Upstream `next_id`: the maximum value of prefix-less `id="number"`
/// attributes in the original XML +1 (1 when none).
///
/// The regex requires whitespace before `id`, so `w:id` / `r:id` (colon
/// directly attached) do not match by mistake. Computed independently for
/// each rendered part (P5, ADR-006: header/footer shape_id is part-scoped).
pub fn shape_id_of(src_xml: &str) -> i64 {
    fn shape_id_re() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new(r#"\sid="([0-9]+)""#).expect("invalid regex"))
    }
    let mut max_id: i64 = 0;
    for captures in shape_id_re().captures_iter(src_xml) {
        if let Some(digits) = captures.get(1) {
            if let Ok(value) = digits.as_str().parse::<i64>() {
                max_id = max_id.max(value);
            }
        }
    }
    max_id.saturating_add(1)
}

/// Image placeholder prefix/suffix (written during conversion; replaced by
/// [`substitute_images`] after rendering).
const IMAGE_TOKEN_PREFIX: &str = "\u{1}DOXTPLRSIMG@";
const IMAGE_TOKEN_SUFFIX: &str = "@\u{1}";

fn image_token_nonce() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let mut first = RandomState::new().build_hasher();
    sequence.hash(&mut first);
    timestamp.hash(&mut first);
    let mut second = RandomState::new().build_hasher();
    timestamp.rotate_left(61).hash(&mut second);
    sequence.rotate_left(29).hash(&mut second);
    format!("{:016x}{:016x}", first.finish(), second.finish())
}

#[derive(Debug)]
pub(crate) struct PendingImages<'a> {
    nonce: String,
    token_re: Regex,
    images: Vec<&'a InlineImage>,
}

impl<'a> PendingImages<'a> {
    fn new() -> Self {
        let nonce = image_token_nonce();
        let token_re = Regex::new(&format!(
            "{}{}@(\\d+){}",
            regex::escape(IMAGE_TOKEN_PREFIX),
            nonce,
            regex::escape(IMAGE_TOKEN_SUFFIX)
        ))
        .expect("nonce image token regex is valid");
        Self {
            nonce,
            token_re,
            images: Vec::new(),
        }
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    fn token(&self, index: usize) -> String {
        format!(
            "{IMAGE_TOKEN_PREFIX}{}@{index}{IMAGE_TOKEN_SUFFIX}",
            self.nonce
        )
    }
}

/// Converts a [`RenderContext`] into a MiniJinja root value.
///
/// InlineImages are not resolved immediately but replaced with placeholders
/// (aligned with the lazy upstream `InlineImage.__str__`: only images
/// actually referenced by the template are resolved within the current
/// part's relationship scope, ADR-006); the returned `pending_images`
/// correspond to placeholders by index and are consumed by
/// [`substitute_images`] in order of appearance in the rendered output.
pub(crate) fn context_to_minijinja<'a>(
    context: &'a RenderContext,
    part: &str,
) -> Result<(Value, PendingImages<'a>), RenderError> {
    let mut pending = PendingImages::new();
    let mut pairs = Vec::with_capacity(context.len());
    for (key, value) in context.iter() {
        pairs.push((
            Value::from(key),
            value_to_minijinja(value, &mut pending)
                .map_err(|message| invalid_context_value(part, message))?,
        ));
    }
    Ok((Value::from_iter(pairs), pending))
}

fn invalid_context_value(part: &str, message: String) -> RenderError {
    RenderError::Template {
        kind: TemplateErrorKind::InvalidArgument,
        part: part.to_string(),
        line: None,
        message,
        context: Vec::new(),
    }
}

/// A docxtpl rich value is a Python object, not a `str`.  It is nevertheless
/// rendered as trusted markup through its `__html__` method and Python object
/// instances stay truthy even when that markup is empty.  A MiniJinja safe
/// string cannot model both properties (an empty safe string is false and is
/// a string), so retain the object identity in this small closed wrapper.
#[derive(Debug)]
struct RichMarkup(String);

impl Object for RichMarkup {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Plain
    }

    fn is_true(self: &Arc<Self>) -> bool {
        true
    }

    fn render(self: &Arc<Self>, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn rich_markup(value: &Value) -> Option<&str> {
    value
        .downcast_object_ref::<RichMarkup>()
        .map(|markup| markup.0.as_str())
}

fn is_markup(value: &Value) -> bool {
    value.is_safe() || rich_markup(value).is_some()
}

fn json_to_minijinja(json: &JsonValue) -> Result<Value, String> {
    match json {
        JsonValue::Null => Ok(Value::from(())),
        JsonValue::Bool(value) => Ok(Value::from(*value)),
        JsonValue::String(value) => Ok(Value::from(value.clone())),
        JsonValue::Array(items) => items
            .iter()
            .map(json_to_minijinja)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::from),
        JsonValue::Object(entries) => {
            let mut pairs = Vec::with_capacity(entries.len());
            for (key, value) in entries {
                pairs.push((Value::from(key.clone()), json_to_minijinja(value)?));
            }
            Ok(Value::from_iter(pairs))
        }
        JsonValue::Number(number) => json_number_to_minijinja(number),
    }
}

fn json_number_to_minijinja(number: &serde_json::Number) -> Result<Value, String> {
    let spelling = number.to_string();
    if !spelling
        .bytes()
        .any(|byte| matches!(byte, b'.' | b'e' | b'E'))
    {
        if spelling.starts_with('-') {
            return spelling.parse::<i128>().map(Value::from).map_err(|_| {
                format!("JSON integer is outside the supported signed 128-bit range: {spelling}")
            });
        }
        return spelling.parse::<i128>().map(Value::from).map_err(|_| {
            format!("JSON integer is outside the supported signed 128-bit range: {spelling}")
        });
    }

    let value = spelling.parse::<f64>().map_err(|_| {
        format!("JSON number cannot be represented by the template engine: {spelling}")
    })?;
    if !value.is_finite() {
        return Err(format!(
            "JSON number is outside the supported finite floating-point range: {spelling}"
        ));
    }
    Ok(Value::from(value))
}

/// Recursively converts a [`RenderValue`] into a MiniJinja value.
///
/// RichText/RichTextParagraph/Listing directly yield the generated XML
/// strings (aligned with upstream `__str__`; autoescape is off by default);
/// InlineImage is registered with `pending` and participates in rendering as
/// a placeholder.
fn value_to_minijinja<'a>(
    value: &'a RenderValue,
    pending: &mut PendingImages<'a>,
) -> Result<Value, String> {
    match value {
        RenderValue::Json(json) => json_to_minijinja(json),
        // All four upstream rich-value kinds implement `__html__`, so they
        // are injected verbatim as Markup even with autoescape on; the
        // object wrapper also preserves Python truthiness/type identity.
        RenderValue::RichText(rich) => Ok(Value::from_object(RichMarkup(rich.to_xml().to_owned()))),
        RenderValue::RichTextParagraph(paragraph) => Ok(Value::from_object(RichMarkup(
            paragraph.to_xml().to_owned(),
        ))),
        RenderValue::Listing(listing) => {
            Ok(Value::from_object(RichMarkup(listing.to_xml().to_owned())))
        }
        RenderValue::Image(image) => {
            let index = pending.images.len();
            let token = pending.token(index);
            pending.images.push(image);
            Ok(Value::from_object(RichMarkup(token)))
        }
        // Subdoc fragments are injected as safe strings (P6, ADR-007):
        // upstream `Subdoc.__html__` exists, so with autoescape on jinja2
        // takes the Markup path without escaping; with it off, output is
        // verbatim and equivalent to `__str__`.
        RenderValue::Subdoc(fragment) => {
            Ok(Value::from_object(RichMarkup(fragment.as_str().to_owned())))
        }
        RenderValue::Array(items) => items
            .iter()
            .map(|item| value_to_minijinja(item, pending))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::from),
        RenderValue::Object(entries) => {
            let mut pairs = Vec::with_capacity(entries.len());
            for (key, item) in entries {
                pairs.push((
                    Value::from(key.as_str()),
                    value_to_minijinja(item, pending)?,
                ));
            }
            Ok(Value::from_iter(pairs))
        }
    }
}

/// Replaces image placeholders in the rendered output with real
/// `wp:inline`/anchor XML.
///
/// Resolves one at a time in the **order of appearance** of placeholders in
/// the output (aligned with the call order of the lazy `__str__` during
/// upstream jinja rendering): the same image appearing multiple times is
/// resolved multiple times — rIds are deduplicated and reused by the registry
/// per (reltype/target/mode); docPr id/name all use the same `shape_id` (see
/// [`shape_id_of`] and the cacheless upstream `StoryPart.next_id`).
/// Placeholders for images not referenced by the template never appear and
/// are therefore never resolved; relationships land only in the scope of
/// the part that actually references them.
#[cfg(test)]
pub(crate) fn substitute_images(
    rendered: &str,
    registry: &mut dyn ImageRegistry,
    pending: &PendingImages<'_>,
    shape_id: i64,
    part: &str,
) -> Result<String, RenderError> {
    substitute_images_with_limit(
        rendered,
        registry,
        pending,
        shape_id,
        part,
        MAX_RENDERED_XML_BYTES,
    )
}

fn push_image_output(
    output: &mut String,
    value: &str,
    part: &str,
    max: usize,
) -> Result<(), RenderError> {
    if value.len() > max.saturating_sub(output.len()) {
        return Err(RenderError::Limit {
            part: part.to_string(),
            kind: "rendered_xml_bytes",
            max: max as u64,
        });
    }
    output.push_str(value);
    Ok(())
}

pub(crate) fn substitute_images_with_limit(
    rendered: &str,
    registry: &mut dyn ImageRegistry,
    pending: &PendingImages<'_>,
    shape_id: i64,
    part: &str,
    max: usize,
) -> Result<String, RenderError> {
    let mut output = String::with_capacity(rendered.len().min(max));
    let mut cursor = 0usize;
    for captures in pending.token_re.captures_iter(rendered) {
        let matched = captures.get(0).expect("image token regex has a full match");
        push_image_output(&mut output, &rendered[cursor..matched.start()], part, max)?;

        let replacement = captures[1]
            .parse::<usize>()
            .ok()
            .and_then(|index| pending.images.get(index).copied());
        if let Some(image) = replacement {
            let rels = registry
                .resolve_image(image)
                .map_err(|err| image_error(&err, part))?;
            let xml = docxtpl_rich::render_inline_image(
                image,
                shape_id,
                &rels.blip_rid,
                rels.hyperlink_rid.as_deref(),
            )
            .map_err(|err| match err {
                docxtpl_rich::ImageError::MetadataTooLarge { .. } => rendered_xml_limit(part),
                docxtpl_rich::ImageError::InvalidXmlMetadata => RenderError::Template {
                    kind: TemplateErrorKind::InvalidArgument,
                    part: part.to_string(),
                    line: None,
                    message: err.to_string(),
                    context: Vec::new(),
                },
                docxtpl_rich::ImageError::Unrecognized => image_error(
                    &ImageResolveError {
                        message: err.to_string(),
                    },
                    part,
                ),
            })?;
            push_image_output(&mut output, &xml, part, max)?;
        } else {
            // A nonce-matching but out-of-range/malformed token is ordinary
            // text.  This keeps substitution closed over the pending table.
            push_image_output(&mut output, matched.as_str(), part, max)?;
        }
        cursor = matched.end();
    }
    push_image_output(&mut output, &rendered[cursor..], part, max)?;
    Ok(output)
}

/// Merges image resolution/rendering failures into
/// `TemplateErrorKind::Image` (oracle exception `UnrecognizedImageError`).
fn image_error(err: &ImageResolveError, part: &str) -> RenderError {
    RenderError::Template {
        kind: TemplateErrorKind::Image,
        part: part.to_string(),
        line: None,
        message: err.message.clone(),
        context: Vec::new(),
    }
}

/// Builds a Jinja environment consistent with upstream: lenient undefined
/// (aligned with the jinja2 default `Undefined`); autoescape takes effect
/// only when explicitly enabled (HTML-rule escaping).
pub(crate) fn build_jinja_env(autoescape: bool) -> Environment<'static> {
    let mut env = Environment::new();
    env.set_fuel(Some(MAX_TEMPLATE_FUEL));
    env.set_undefined_behavior(UndefinedBehavior::Lenient);
    // MiniJinja's built-in versions deliberately use compact JSON/insertion
    // order and RFC3986 `%20`, whereas Jinja2's default tojson sorts keys,
    // keeps separator spaces, and uses ensure_ascii, and the mapping
    // urlencode uses the application/x-www-form-urlencoded `+`. Override
    // these two filters to narrow common output to Python Jinja2.
    env.add_filter("tojson", jinja_tojson);
    env.add_filter("urlencode", jinja_urlencode);
    env.add_filter("escape", jinja_escape_filter);
    env.add_filter("e", jinja_escape_filter);
    env.add_filter("forceescape", jinja_forceescape_filter);
    env.add_filter("xmlattr", jinja_xmlattr);
    // These builtins stringify values internally, before the environment
    // formatter gets a chance to apply Python spelling.  Override the common
    // paths so containers, booleans, None and floats use Python `str` just as
    // Jinja2 does.
    env.add_filter("string", jinja_string_filter);
    env.add_filter("join", jinja_join_filter);
    env.add_filter("replace", jinja_replace_filter);
    env.add_filter("format", jinja_format_filter);
    env.add_filter("center", jinja_center_filter);
    env.add_filter("filesizeformat", jinja_filesizeformat_filter);
    env.add_filter("striptags", jinja_striptags_filter);
    env.add_filter("truncate", jinja_truncate_filter);
    env.add_filter("wordcount", jinja_wordcount_filter);
    env.add_filter("wordwrap", jinja_wordwrap_filter);
    env.add_filter("random", minijinja_contrib::filters::random);
    env.add_function("cycler", jinja_cycler);
    env.add_function("joiner", jinja_joiner);
    env.add_function("lipsum", minijinja_contrib::globals::lipsum);
    env.add_function("randrange", minijinja_contrib::globals::randrange);
    env.add_test("callable", jinja_is_callable);
    env.add_test("escaped", is_markup);
    env.add_test("safe", is_markup);
    env.add_test("number", jinja_is_number);
    env.add_test("sequence", jinja_is_sequence);
    env.set_unknown_method_callback(jinja_python_method);
    env.set_formatter(|out, state, value| {
        if let Some(markup) = rich_markup(value) {
            return out.write_str(markup).map_err(Error::from);
        }
        match (state.auto_escape(), value.is_safe()) {
            (AutoEscape::Html, false) => out
                .write_str(&jinja_html_escape(&python_display(value)?)?)
                .map_err(Error::from),
            // MiniJinja's default Display uses JSON quotes for containers and
            // Rust's float exponent spelling.  Jinja renders values through
            // Python str even when autoescape is disabled.
            (AutoEscape::None, _) => out.write_str(&python_display(value)?).map_err(Error::from),
            _ => escape_formatter(out, state, value),
        }
    });
    if autoescape {
        env.set_auto_escape_callback(|_| minijinja::AutoEscape::Html);
    }
    env
}

fn jinja_is_number(value: &Value) -> bool {
    matches!(value.kind(), ValueKind::Bool | ValueKind::Number)
}

fn jinja_is_sequence(value: &Value) -> bool {
    matches!(
        value.kind(),
        ValueKind::String | ValueKind::Seq | ValueKind::Map
    )
}

pub(crate) fn build_jinja_env_with_options(options: &RenderOptions) -> Environment<'static> {
    let mut environment = build_jinja_env(options.autoescape());
    environment.set_fuel(Some(options.template_fuel()));
    options.apply_environment_configurator(&mut environment);
    environment
}

fn jinja_escape_filter(value: &Value) -> Result<Value, Error> {
    if let Some(markup) = rich_markup(value) {
        return Ok(Value::from_safe_string(markup.to_owned()));
    }
    if value.is_safe() {
        return Ok(value.clone());
    }
    Ok(Value::from_safe_string(jinja_html_escape(
        &python_display(value)?,
    )?))
}

fn jinja_forceescape_filter(value: &Value) -> Result<Value, Error> {
    Ok(Value::from_safe_string(jinja_html_escape(
        &python_display(value)?,
    )?))
}

fn jinja_string_filter(value: &Value) -> Result<Value, Error> {
    // MarkupSafe.soft_str preserves an already-safe string.  All other values
    // are converted through Python's `str` spelling.
    if let Some(markup) = rich_markup(value) {
        // RichText implements `__html__`, but it is not itself a Markup
        // string.  Jinja's `string`/soft_str therefore calls `str()` and the
        // resulting ordinary string is escaped when autoescape is enabled.
        Ok(Value::from(markup.to_owned()))
    } else if value.kind() == ValueKind::String {
        Ok(value.clone())
    } else {
        Ok(Value::from(python_display(value)?))
    }
}

fn invalid_operation(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidOperation, message.into())
}

/// Marker carried through MiniJinja's error source chain for operations that
/// Python/Jinja reports as `ValueError`.  MiniJinja groups these together with
/// other invalid operations, so the marker lets the public error taxonomy keep
/// the upstream exception category without parsing human-readable messages.
#[derive(Debug)]
struct PythonValueError;

impl fmt::Display for PythonValueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Python ValueError")
    }
}

impl std::error::Error for PythonValueError {}

fn invalid_value(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidOperation, message.into()).with_source(PythonValueError)
}

fn is_python_value_error(error: &minijinja::Error) -> bool {
    let mut current = std::error::Error::source(error);
    while let Some(source) = current {
        if source.downcast_ref::<PythonValueError>().is_some() {
            return true;
        }
        current = source.source();
    }
    false
}

fn require_arity(name: &str, args: &[Value], min: usize, max: usize) -> Result<(), Error> {
    if args.len() < min {
        return Err(Error::new(
            ErrorKind::MissingArgument,
            format!("{name} expected at least {min} argument(s)"),
        ));
    }
    if args.len() > max {
        return Err(Error::new(
            ErrorKind::TooManyArguments,
            format!("{name} expected at most {max} argument(s)"),
        ));
    }
    Ok(())
}

fn integer_argument(name: &str, value: &Value) -> Result<i64, Error> {
    if value.kind() == ValueKind::Bool {
        return Ok(i64::from(bool::try_from(value.clone())?));
    }
    i64::try_from(value.clone())
        .map_err(|_| invalid_operation(format!("{name} must be an integer")))
}

fn normalized_slice_index(index: i64, len: usize) -> usize {
    if index < 0 {
        len.saturating_sub(index.unsigned_abs().min(len as u64) as usize)
    } else {
        usize::try_from(index).unwrap_or(usize::MAX).min(len)
    }
}

/// Closed Python-method compatibility surface for values originating in the
/// JSON/render context.  No arbitrary host methods are exposed.
fn jinja_python_method(
    _state: &State,
    value: &Value,
    method: &str,
    args: &[Value],
) -> Result<Value, Error> {
    match (value.kind(), method) {
        (ValueKind::Seq, "index") => {
            require_arity("index", args, 1, 3)?;
            let items = value.try_iter()?.collect::<Vec<_>>();
            let start = args
                .get(1)
                .map(|arg| integer_argument("index start", arg))
                .transpose()?
                .map_or(0, |index| normalized_slice_index(index, items.len()));
            let stop = args
                .get(2)
                .map(|arg| integer_argument("index stop", arg))
                .transpose()?
                .map_or(items.len(), |index| {
                    normalized_slice_index(index, items.len())
                });
            items[start.min(stop)..stop]
                .iter()
                .position(|item| item == &args[0])
                .map(|index| Value::from((start + index) as u64))
                .ok_or_else(|| invalid_value("sequence.index(x): x not in sequence"))
        }
        (ValueKind::String, "split") => python_string_split(value, args),
        (ValueKind::String, "startswith") => python_string_affix(value, args, true),
        (ValueKind::String, "endswith") => python_string_affix(value, args, false),
        (ValueKind::String, "count") => python_string_count(value, args),
        (ValueKind::String, "index") => python_string_index(value, args),
        _ => minijinja_contrib::pycompat::unknown_method_callback(_state, value, method, args),
    }
}

fn python_string_split(value: &Value, args: &[Value]) -> Result<Value, Error> {
    let (positional, kwargs): (&[Value], Kwargs) = from_args(args)?;
    require_arity("split", positional, 0, 2)?;
    let text = value.as_str().unwrap_or_default();
    if text.len() > MAX_RENDERED_XML_BYTES {
        return Err(invalid_operation(
            "split input exceeds the rendered XML limit",
        ));
    }
    let separator = filter_argument("split", positional, 0, &kwargs, "sep")?;
    let maxsplit = filter_argument("split", positional, 1, &kwargs, "maxsplit")?
        .as_ref()
        .map(|arg| integer_argument("split maxsplit", arg))
        .transpose()?
        .unwrap_or(-1);
    kwargs.assert_all_used()?;
    let parts = match separator.as_ref() {
        None => python_whitespace_split(text, maxsplit)?,
        Some(separator) if separator.is_none() => python_whitespace_split(text, maxsplit)?,
        Some(separator) => {
            let Some(separator) = separator.as_str() else {
                return Err(invalid_operation(
                    "split separator must be a string or none",
                ));
            };
            if separator.is_empty() {
                return Err(invalid_value("empty separator"));
            }
            if maxsplit < 0 {
                collect_split_parts(text.split(separator))?
            } else {
                let limit = usize::try_from(maxsplit)
                    .unwrap_or(usize::MAX)
                    .saturating_add(1);
                collect_split_parts(text.splitn(limit, separator))?
            }
        }
    };
    Ok(Value::from(parts))
}

fn collect_split_parts<'a>(parts: impl IntoIterator<Item = &'a str>) -> Result<Vec<String>, Error> {
    collect_split_parts_with_limit(parts, MAX_TEMPLATE_INTERMEDIATE_ITEMS)
}

fn collect_split_parts_with_limit<'a>(
    parts: impl IntoIterator<Item = &'a str>,
    max_items: usize,
) -> Result<Vec<String>, Error> {
    let mut collected = Vec::new();
    for part in parts {
        if collected.len() >= max_items {
            return Err(invalid_operation(
                "split intermediate items exceed the rendered XML limit",
            ));
        }
        collected.push(part.to_owned());
    }
    Ok(collected)
}

fn push_split_part(parts: &mut Vec<String>, part: &str) -> Result<(), Error> {
    if parts.len() >= MAX_TEMPLATE_INTERMEDIATE_ITEMS {
        return Err(invalid_operation(
            "split intermediate items exceed the rendered XML limit",
        ));
    }
    parts.push(part.to_owned());
    Ok(())
}

fn python_whitespace_split(text: &str, maxsplit: i64) -> Result<Vec<String>, Error> {
    if maxsplit < 0 {
        return collect_split_parts(text.split_whitespace());
    }

    let mut parts = Vec::new();
    let mut cursor = 0;
    let skip_whitespace = |start: usize| {
        text[start..]
            .char_indices()
            .find(|(_, ch)| !ch.is_whitespace())
            .map_or(text.len(), |(index, _)| start + index)
    };
    cursor = skip_whitespace(cursor);
    if cursor == text.len() {
        return Ok(parts);
    }
    if maxsplit == 0 {
        push_split_part(&mut parts, &text[cursor..])?;
        return Ok(parts);
    }

    for _ in 0..maxsplit {
        let end = text[cursor..]
            .char_indices()
            .find(|(_, ch)| ch.is_whitespace())
            .map_or(text.len(), |(index, _)| cursor + index);
        push_split_part(&mut parts, &text[cursor..end])?;
        cursor = skip_whitespace(end);
        if cursor == text.len() {
            return Ok(parts);
        }
    }
    push_split_part(&mut parts, &text[cursor..])?;
    Ok(parts)
}

fn string_slice_bounds(
    text: &str,
    args: &[Value],
    offset: usize,
) -> Result<(usize, usize, bool), Error> {
    let len = text.chars().count();
    let raw_start = args
        .get(offset)
        .map(|arg| integer_argument("string slice start", arg))
        .transpose()?;
    let start = raw_start.map_or(0, |index| normalized_slice_index(index, len));
    let end = args
        .get(offset + 1)
        .map(|arg| integer_argument("string slice end", arg))
        .transpose()?
        .map_or(len, |index| normalized_slice_index(index, len));
    let start_is_in_range = raw_start.is_none_or(|index| index < 0 || index as u64 <= len as u64);
    Ok((start, end, start_is_in_range && start <= end))
}

fn char_slice(text: &str, start: usize, end: usize) -> String {
    text.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

fn python_string_affix(value: &Value, args: &[Value], starts: bool) -> Result<Value, Error> {
    let name = if starts { "startswith" } else { "endswith" };
    require_arity(name, args, 1, 3)?;
    let text = value.as_str().unwrap_or_default();
    let (start, end, valid_range) = string_slice_bounds(text, args, 1)?;
    if !valid_range {
        return Ok(Value::from(false));
    }
    let slice = char_slice(text, start, end);
    let matches = |needle: &str| {
        if starts {
            slice.starts_with(needle)
        } else {
            slice.ends_with(needle)
        }
    };
    if let Some(needle) = args[0].as_str() {
        return Ok(Value::from(matches(needle)));
    }
    if args[0].kind() != ValueKind::Seq {
        return Err(invalid_operation(format!(
            "{name} prefix/suffix must be a string or tuple of strings"
        )));
    }
    for needle in args[0].try_iter()? {
        let Some(needle) = needle.as_str() else {
            return Err(invalid_operation(format!(
                "{name} tuple items must be strings"
            )));
        };
        // CPython checks tuple entries left-to-right and returns immediately
        // on a match, even if a later entry has the wrong type.
        if matches(needle) {
            return Ok(Value::from(true));
        }
    }
    Ok(Value::from(false))
}

fn python_string_count(value: &Value, args: &[Value]) -> Result<Value, Error> {
    require_arity("count", args, 1, 3)?;
    let Some(needle) = args[0].as_str() else {
        return Err(invalid_operation("count substring must be a string"));
    };
    let text = value.as_str().unwrap_or_default();
    let (start, end, valid_range) = string_slice_bounds(text, args, 1)?;
    if !valid_range {
        return Ok(Value::from(0_u64));
    }
    let slice = char_slice(text, start, end);
    let count = if needle.is_empty() {
        slice.chars().count() + 1
    } else {
        slice.match_indices(needle).count()
    };
    Ok(Value::from(count as u64))
}

fn python_string_index(value: &Value, args: &[Value]) -> Result<Value, Error> {
    require_arity("index", args, 1, 3)?;
    let Some(needle) = args[0].as_str() else {
        return Err(invalid_operation("index substring must be a string"));
    };
    let text = value.as_str().unwrap_or_default();
    let (start, end, valid_range) = string_slice_bounds(text, args, 1)?;
    if !valid_range {
        return Err(invalid_value("substring not found"));
    }
    let slice = char_slice(text, start, end);
    let byte_index = slice
        .find(needle)
        .ok_or_else(|| invalid_value("substring not found"))?;
    let char_index = slice[..byte_index].chars().count();
    Ok(Value::from((start + char_index) as u64))
}

fn jinja_center_filter(value: &Value, args: Rest<Value>) -> Result<Value, Error> {
    let (positional, kwargs): (&[Value], Kwargs) = from_args(&args)?;
    require_arity("center", positional, 0, 1)?;
    let width = filter_argument("center", positional, 0, &kwargs, "width")?
        .as_ref()
        .map(|value| integer_argument("center width", value))
        .transpose()?
        .unwrap_or(80)
        .max(0) as usize;
    kwargs.assert_all_used()?;
    if width > MAX_RENDERED_XML_BYTES {
        return Err(invalid_operation(
            "center width exceeds the rendered XML limit",
        ));
    }
    let text = python_display(value)?;
    if text.len() > MAX_RENDERED_XML_BYTES {
        return Err(invalid_operation(
            "center input exceeds the rendered XML limit",
        ));
    }
    let padding = width.saturating_sub(text.chars().count());
    let output_len = text
        .len()
        .checked_add(padding)
        .filter(|length| *length <= MAX_RENDERED_XML_BYTES)
        .ok_or_else(|| invalid_operation("center output exceeds the rendered XML limit"))?;
    let left = padding / 2;
    let mut output = String::with_capacity(output_len);
    output.extend(std::iter::repeat_n(' ', left));
    output.push_str(&text);
    output.extend(std::iter::repeat_n(' ', padding - left));
    // RichText-like values expose __html__, but are not Markup strings.
    // soft_str(value).center(...) therefore produces an ordinary string.
    Ok(if value.is_safe() {
        Value::from_safe_string(output)
    } else {
        Value::from(output)
    })
}

fn jinja_filesizeformat_filter(value: &Value, args: Rest<Value>) -> Result<String, Error> {
    let (positional, kwargs): (&[Value], Kwargs) = from_args(&args)?;
    require_arity("filesizeformat", positional, 0, 1)?;
    let binary = filter_argument("filesizeformat", positional, 0, &kwargs, "binary")?
        .is_some_and(|value| value.is_true());
    kwargs.assert_all_used()?;
    // Python's float(True/False) is 1.0/0.0; Python spelling ("True") is not
    // itself parseable as a float, so handle booleans before the numeric path.
    let bytes = match value.kind() {
        ValueKind::Bool => {
            if bool::try_from(value.clone())? {
                1.0
            } else {
                0.0
            }
        }
        ValueKind::Number => f64::try_from(value.clone())
            .map_err(|_| invalid_operation("filesizeformat value must be numeric"))?,
        ValueKind::String => value
            .as_str()
            .unwrap_or_default()
            .trim()
            .parse::<f64>()
            .map_err(|_| invalid_value("filesizeformat value must be numeric"))?,
        ValueKind::Undefined => {
            return Err(Error::new(
                ErrorKind::UndefinedError,
                "filesizeformat value is undefined",
            ));
        }
        _ => {
            return Err(invalid_operation(
                "filesizeformat value must be a string or real number",
            ));
        }
    };
    let base: f64 = if binary { 1024.0 } else { 1000.0 };
    if bytes == 1.0 {
        return Ok("1 Byte".to_string());
    }
    if bytes < base {
        return Ok(format!("{} Bytes", bytes as i64));
    }
    let prefixes = if binary {
        ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB", "ZiB", "YiB"]
    } else {
        ["kB", "MB", "GB", "TB", "PB", "EB", "ZB", "YB"]
    };
    for (index, prefix) in prefixes.iter().enumerate() {
        let unit = base.powi(index as i32 + 2);
        if bytes < unit {
            return Ok(format!("{:.1} {prefix}", base * bytes / unit));
        }
    }
    let unit = base.powi(prefixes.len() as i32 + 1);
    Ok(format!("{:.1} {}", base * bytes / unit, prefixes[7]))
}

fn strip_markup_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)<!--.*?-->|<[^>]*>").expect("valid tag regex"))
}

fn jinja_striptags_filter(value: &Value) -> Result<String, Error> {
    // MarkupSafe collapses whitespace before unescaping entities, so an
    // encoded non-breaking space must survive the collapse. The final decoder
    // supplies the complete HTML5 named-entity table.
    let text = python_display(value)?;
    if text.len() > MAX_RENDERED_XML_BYTES {
        return Err(invalid_operation(
            "striptags input exceeds the rendered XML limit",
        ));
    }
    let without_tags = strip_markup_re().replace_all(&text, "");
    let mut collapsed = String::with_capacity(without_tags.len());
    for word in without_tags.split_whitespace() {
        if !collapsed.is_empty() {
            push_filter_output(&mut collapsed, " ", "striptags", MAX_RENDERED_XML_BYTES)?;
        }
        push_filter_output(&mut collapsed, word, "striptags", MAX_RENDERED_XML_BYTES)?;
    }
    let output = html_escape::decode_html_entities(&collapsed).into_owned();
    if output.len() > MAX_RENDERED_XML_BYTES {
        return Err(invalid_operation(
            "striptags output exceeds the rendered XML limit",
        ));
    }
    Ok(output)
}

fn jinja_truncate_filter(_state: &State, value: &Value, args: Rest<Value>) -> Result<Value, Error> {
    let (positional, kwargs): (&[Value], Kwargs) = from_args(&args)?;
    if positional.len() > 4 {
        return Err(Error::from(ErrorKind::TooManyArguments));
    }
    let length = filter_argument("truncate", positional, 0, &kwargs, "length")?
        .unwrap_or_else(|| Value::from(255));
    let killwords = filter_argument("truncate", positional, 1, &kwargs, "killwords")?
        .unwrap_or_else(|| Value::from(false));
    let end = filter_argument("truncate", positional, 2, &kwargs, "end")?
        .unwrap_or_else(|| Value::from("..."));
    let leeway = filter_argument("truncate", positional, 3, &kwargs, "leeway")?
        .filter(|value| !value.is_none())
        .unwrap_or_else(|| Value::from(5));
    kwargs.assert_all_used()?;
    let length_number = usize::try_from(length.clone())
        .map_err(|_| invalid_operation("truncate length must be a non-negative integer"))?;
    let leeway_number = usize::try_from(leeway.clone())
        .map_err(|_| invalid_operation("truncate leeway must be a non-negative integer"))?;
    if length_number > MAX_RENDERED_XML_BYTES
        || leeway_number > MAX_RENDERED_XML_BYTES
        || length_number.saturating_add(leeway_number) > MAX_RENDERED_XML_BYTES
    {
        return Err(invalid_operation(
            "truncate arguments exceed the rendered XML limit",
        ));
    }
    if let Some(input) = value.as_str() {
        if input.len() > MAX_RENDERED_XML_BYTES {
            return Err(invalid_operation(
                "truncate input exceeds the rendered XML limit",
            ));
        }
    }
    let end_text = end
        .as_str()
        .ok_or_else(|| invalid_operation("truncate end must be a string"))?;
    if end_text.len() > MAX_RENDERED_XML_BYTES {
        return Err(invalid_operation(
            "truncate end exceeds the rendered XML limit",
        ));
    }
    let output = minijinja_contrib::filters::truncate(
        _state,
        value,
        Kwargs::from_iter([
            ("length", length),
            ("killwords", killwords),
            ("end", end),
            ("leeway", leeway),
        ]),
    )?;
    if output
        .as_str()
        .is_some_and(|text| text.len() > MAX_RENDERED_XML_BYTES)
    {
        return Err(invalid_operation(
            "truncate output exceeds the rendered XML limit",
        ));
    }
    Ok(output)
}

fn jinja_wordcount_filter(value: &Value) -> Result<Value, Error> {
    // Jinja stringifies non-string values before applying its word regex.
    // contrib's `as_str().unwrap_or_default()` would incorrectly report zero.
    minijinja_contrib::filters::wordcount(&Value::from(python_display(value)?))
}

#[derive(Clone, Copy, Debug)]
struct PythonWrapChunk<'a> {
    text: &'a str,
    chars: usize,
    whitespace: bool,
}

fn textwrap_whitespace(ch: char) -> bool {
    matches!(ch, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r')
}

fn push_wordwrap_chunk<'a>(
    chunks: &mut VecDeque<PythonWrapChunk<'a>>,
    text: &'a str,
    chars: usize,
    whitespace: bool,
) -> Result<(), Error> {
    // VecDeque may reserve up to roughly twice its current length.  Keep that
    // backing allocation below the same 600 MiB budget as rendered XML.
    let max_chunks =
        MAX_RENDERED_XML_BYTES / (2 * std::mem::size_of::<PythonWrapChunk<'_>>().max(1));
    if chunks.len() >= max_chunks {
        return Err(invalid_operation(
            "wordwrap intermediate chunks exceed the rendered XML limit",
        ));
    }
    chunks.push_back(PythonWrapChunk {
        text,
        chars,
        whitespace,
    });
    Ok(())
}

fn push_hyphenated_chunks<'a>(
    chunks: &mut VecDeque<PythonWrapChunk<'a>>,
    word: &'a str,
) -> Result<(), Error> {
    let mut start = 0;
    let mut chars_since_start = 0;
    let mut previous = [None, None, None];
    for (byte_index, ch) in word.char_indices() {
        chars_since_start += 1;
        if ch == '-' {
            let mut following = word[byte_index + 1..].chars();
            let next = following.next();
            let after_next = following.next();
            let suffix_matches = next.is_some_and(char::is_alphabetic)
                && (after_next.is_some_and(char::is_alphabetic)
                    || (after_next == Some('-')
                        && following.next().is_some_and(char::is_alphabetic)));
            let prefix_matches = previous[0].is_some_and(char::is_alphabetic)
                && (previous[1].is_some_and(char::is_alphabetic)
                    || (previous[1] == Some('-') && previous[2].is_some_and(char::is_alphabetic)));
            if prefix_matches && suffix_matches {
                let end = byte_index + 1;
                push_wordwrap_chunk(chunks, &word[start..end], chars_since_start, false)?;
                start = end;
                chars_since_start = 0;
            }
        }
        previous = [Some(ch), previous[0], previous[1]];
    }
    if start < word.len() {
        push_wordwrap_chunk(chunks, &word[start..], chars_since_start, false)?;
    }
    Ok(())
}

fn python_wordwrap_chunks(
    paragraph: &str,
    break_on_hyphens: bool,
) -> Result<VecDeque<PythonWrapChunk<'_>>, Error> {
    let mut chunks = VecDeque::new();
    let mut indices = paragraph.char_indices();
    let Some((_, first)) = indices.next() else {
        return Ok(chunks);
    };
    let mut start = 0;
    let mut run_chars = 1;
    let mut whitespace = textwrap_whitespace(first);
    for (byte_index, ch) in indices {
        let next_whitespace = textwrap_whitespace(ch);
        if next_whitespace == whitespace {
            run_chars += 1;
            continue;
        }
        let run = &paragraph[start..byte_index];
        if break_on_hyphens && !whitespace {
            push_hyphenated_chunks(&mut chunks, run)?;
        } else {
            push_wordwrap_chunk(&mut chunks, run, run_chars, whitespace)?;
        }
        start = byte_index;
        run_chars = 1;
        whitespace = next_whitespace;
    }
    let run = &paragraph[start..];
    if break_on_hyphens && !whitespace {
        push_hyphenated_chunks(&mut chunks, run)?;
    } else {
        push_wordwrap_chunk(&mut chunks, run, run_chars, whitespace)?;
    }
    Ok(chunks)
}

fn split_chunk_at_chars(
    chunk: PythonWrapChunk<'_>,
    end: usize,
) -> (PythonWrapChunk<'_>, PythonWrapChunk<'_>) {
    debug_assert!(end > 0 && end < chunk.chars);
    let byte_index = chunk
        .text
        .char_indices()
        .nth(end)
        .map(|(index, _)| index)
        .unwrap_or(chunk.text.len());
    (
        PythonWrapChunk {
            text: &chunk.text[..byte_index],
            chars: end,
            whitespace: chunk.whitespace,
        },
        PythonWrapChunk {
            text: &chunk.text[byte_index..],
            chars: chunk.chars - end,
            whitespace: chunk.whitespace,
        },
    )
}

fn python_hyphen_break(chunk: PythonWrapChunk<'_>, space_left: usize) -> Option<usize> {
    if space_left >= chunk.chars {
        return None;
    }
    let mut last_hyphen = None;
    let mut non_hyphen_before = false;
    for (index, ch) in chunk.text.chars().take(space_left).enumerate() {
        if ch == '-' && index > 0 && non_hyphen_before {
            last_hyphen = Some(index + 1);
        }
        if ch != '-' {
            non_hyphen_before = true;
        }
    }
    last_hyphen
}

fn push_wordwrap_output(output: &mut String, value: &str) -> Result<(), Error> {
    push_filter_output(output, value, "wordwrap", MAX_RENDERED_XML_BYTES)
}

fn wrap_python_paragraph(
    paragraph: &str,
    width: usize,
    break_long_words: bool,
    break_on_hyphens: bool,
    wrapstring: &str,
    output: &mut String,
) -> Result<(), Error> {
    let mut chunks = python_wordwrap_chunks(paragraph, break_on_hyphens)?;
    let mut line_number = 0usize;
    while !chunks.is_empty() {
        let mut line = String::new();
        let mut line_chars = 0usize;
        let mut trailing_whitespace = None;

        if line_number > 0 && chunks.front().is_some_and(|chunk| chunk.whitespace) {
            chunks.pop_front();
        }
        while let Some(chunk) = chunks.front().copied() {
            if line_chars.saturating_add(chunk.chars) > width {
                break;
            }
            chunks.pop_front();
            push_filter_output(&mut line, chunk.text, "wordwrap", MAX_RENDERED_XML_BYTES)?;
            line_chars += chunk.chars;
            trailing_whitespace = chunk.whitespace.then_some((chunk.text.len(), chunk.chars));
        }

        if chunks.front().is_some_and(|chunk| chunk.chars > width) {
            let chunk = chunks.pop_front().expect("front was present");
            let space_left = width.saturating_sub(line_chars);
            if break_long_words && space_left > 0 {
                let end = if break_on_hyphens {
                    python_hyphen_break(chunk, space_left).unwrap_or(space_left)
                } else {
                    space_left
                };
                let (prefix, remainder) = split_chunk_at_chars(chunk, end);
                push_filter_output(&mut line, prefix.text, "wordwrap", MAX_RENDERED_XML_BYTES)?;
                trailing_whitespace = prefix
                    .whitespace
                    .then_some((prefix.text.len(), prefix.chars));
                chunks.push_front(remainder);
            } else if line.is_empty() {
                push_filter_output(&mut line, chunk.text, "wordwrap", MAX_RENDERED_XML_BYTES)?;
                trailing_whitespace = chunk.whitespace.then_some((chunk.text.len(), chunk.chars));
            } else {
                chunks.push_front(chunk);
            }
        }

        if let Some((bytes, _chars)) = trailing_whitespace {
            line.truncate(line.len().saturating_sub(bytes));
        }
        if line.is_empty() {
            continue;
        }
        if line_number > 0 {
            push_wordwrap_output(output, wrapstring)?;
        }
        push_wordwrap_output(output, &line)?;
        line_number += 1;
    }
    Ok(())
}

fn python_splitlines(value: &str) -> Result<Vec<&str>, Error> {
    let mut lines = Vec::new();
    let max_lines = MAX_RENDERED_XML_BYTES / (2 * std::mem::size_of::<&str>().max(1));
    let mut start = 0usize;
    let mut chars = value.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        let is_boundary = matches!(
            ch,
            '\n' | '\r'
                | '\u{b}'
                | '\u{c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if !is_boundary {
            continue;
        }
        if lines.len() >= max_lines {
            return Err(invalid_operation(
                "wordwrap paragraph index exceeds the rendered XML limit",
            ));
        }
        lines.push(&value[start..index]);
        start = index + ch.len_utf8();
        if ch == '\r' && chars.peek().is_some_and(|(_, next)| *next == '\n') {
            let (next_index, next) = chars.next().expect("peeked line feed");
            start = next_index + next.len_utf8();
        }
    }
    if start < value.len() {
        if lines.len() >= max_lines {
            return Err(invalid_operation(
                "wordwrap paragraph index exceeds the rendered XML limit",
            ));
        }
        lines.push(&value[start..]);
    }
    Ok(lines)
}

fn jinja_wordwrap_filter(value: &Value, args: Rest<Value>) -> Result<Value, Error> {
    let (positional, kwargs): (&[Value], Kwargs) = from_args(&args)?;
    require_arity("wordwrap", positional, 0, 4)?;
    let width = filter_argument("wordwrap", positional, 0, &kwargs, "width")?
        .unwrap_or_else(|| Value::from(79));
    let break_long_words = filter_argument("wordwrap", positional, 1, &kwargs, "break_long_words")?
        .unwrap_or_else(|| Value::from(true));
    let wrapstring = filter_argument("wordwrap", positional, 2, &kwargs, "wrapstring")?
        .filter(|value| !value.is_none())
        .unwrap_or_else(|| Value::from("\n"));
    let break_on_hyphens = filter_argument("wordwrap", positional, 3, &kwargs, "break_on_hyphens")?
        .unwrap_or_else(|| Value::from(true));
    kwargs.assert_all_used()?;
    let width_number = i64::try_from(width.clone())
        .map_err(|_| invalid_operation("wordwrap width must be a positive integer"))?;
    if width_number <= 0 {
        return Err(invalid_value(format!(
            "invalid width {width_number} (must be > 0)"
        )));
    }
    let width_number = width_number as usize;
    if width_number > MAX_RENDERED_XML_BYTES {
        return Err(invalid_operation(
            "wordwrap width must be within the rendered XML limit",
        ));
    }
    let input = value
        .as_str()
        .ok_or_else(|| invalid_operation("wordwrap input must be a string"))?;
    if input.len() > MAX_RENDERED_XML_BYTES {
        return Err(invalid_operation(
            "wordwrap input exceeds the rendered XML limit",
        ));
    }
    let wrapstring_text = wrapstring
        .as_str()
        .ok_or_else(|| invalid_operation("wordwrap wrapstring must be a string"))?;
    if wrapstring_text.len() > MAX_RENDERED_XML_BYTES {
        return Err(invalid_operation(
            "wordwrap wrapstring exceeds the rendered XML limit",
        ));
    }
    let break_long_words = bool::try_from(break_long_words)
        .map_err(|_| invalid_operation("wordwrap break_long_words must be a boolean"))?;
    let break_on_hyphens = bool::try_from(break_on_hyphens)
        .map_err(|_| invalid_operation("wordwrap break_on_hyphens must be a boolean"))?;

    let paragraphs = python_splitlines(input)?;
    let mut output = String::with_capacity(input.len().min(MAX_RENDERED_XML_BYTES));
    for (index, paragraph) in paragraphs.into_iter().enumerate() {
        if index > 0 {
            push_wordwrap_output(&mut output, wrapstring_text)?;
        }
        wrap_python_paragraph(
            paragraph,
            width_number,
            break_long_words,
            break_on_hyphens,
            wrapstring_text,
            &mut output,
        )?;
    }
    Ok(Value::from(output))
}

#[derive(Debug)]
struct Cycler {
    items: Vec<Value>,
    index: AtomicUsize,
}

impl Object for Cycler {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Plain
    }

    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        match key.as_str()? {
            "current" => {
                Some(self.items[self.index.load(Ordering::Relaxed) % self.items.len()].clone())
            }
            _ => None,
        }
    }

    fn call_method(
        self: &Arc<Self>,
        _state: &State,
        method: &str,
        args: &[Value],
    ) -> Result<Value, Error> {
        match method {
            "next" => {
                require_arity("cycler.next", args, 0, 0)?;
                let index = self.index.fetch_add(1, Ordering::Relaxed) % self.items.len();
                Ok(self.items[index].clone())
            }
            "reset" => {
                require_arity("cycler.reset", args, 0, 0)?;
                self.index.store(0, Ordering::Relaxed);
                Ok(Value::from(()))
            }
            _ => Err(Error::from(ErrorKind::UnknownMethod)),
        }
    }

    fn render(self: &Arc<Self>, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<jinja2.utils.Cycler object>")
    }
}

fn jinja_cycler(args: Rest<Value>) -> Result<Value, Error> {
    let (positional, kwargs): (&[Value], Kwargs) = from_args(&args)?;
    kwargs.assert_all_used()?;
    if positional.is_empty() {
        return Err(invalid_operation("at least one item has to be provided"));
    }
    Ok(Value::from_object(Cycler {
        items: positional.to_vec(),
        index: AtomicUsize::new(0),
    }))
}

#[derive(Debug)]
struct Joiner {
    separator: Value,
    calls: AtomicUsize,
}

impl Object for Joiner {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Plain
    }

    fn call(self: &Arc<Self>, _state: &State, args: &[Value]) -> Result<Value, Error> {
        require_arity("joiner", args, 0, 0)?;
        if self.calls.fetch_add(1, Ordering::Relaxed) == 0 {
            Ok(Value::from(""))
        } else {
            Ok(self.separator.clone())
        }
    }

    fn render(self: &Arc<Self>, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<jinja2.utils.Joiner object>")
    }
}

fn jinja_joiner(args: Rest<Value>) -> Result<Value, Error> {
    let (positional, kwargs): (&[Value], Kwargs) = from_args(&args)?;
    require_arity("joiner", positional, 0, 1)?;
    let separator = filter_argument("joiner", positional, 0, &kwargs, "sep")?
        .unwrap_or_else(|| Value::from(", "));
    kwargs.assert_all_used()?;
    Ok(Value::from_object(Joiner {
        separator,
        calls: AtomicUsize::new(0),
    }))
}

fn jinja_is_callable(value: &Value) -> bool {
    // Jinja's Undefined object implements __call__; invoking it still raises,
    // but the `callable` test itself returns true.
    if value.kind() == ValueKind::Undefined {
        return true;
    }
    if value.downcast_object_ref::<Joiner>().is_some() {
        return true;
    }
    if value.downcast_object_ref::<RichMarkup>().is_some()
        || value.downcast_object_ref::<Cycler>().is_some()
    {
        return false;
    }
    if value.kind() == ValueKind::Plain {
        return true;
    }
    // MiniJinja's VM macro object intentionally advertises a map
    // representation even though it implements Object::call.  There is no
    // public callable-introspection hook, but its stable Debug representation
    // lets us distinguish it from JSON maps and namespace() instances without
    // invoking the value (which could have side effects).
    value.as_object().is_some_and(|object| {
        struct DebugPrefix {
            value: String,
            max: usize,
        }

        impl fmt::Write for DebugPrefix {
            fn write_str(&mut self, text: &str) -> fmt::Result {
                let remaining = self.max.saturating_sub(self.value.len());
                let mut end = text.len().min(remaining);
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                self.value.push_str(&text[..end]);
                if text.len() > remaining || self.value.len() >= self.max {
                    Err(fmt::Error)
                } else {
                    Ok(())
                }
            }
        }

        let expected = "<macro ";
        let mut prefix = DebugPrefix {
            value: String::with_capacity(expected.len()),
            max: expected.len(),
        };
        let _ = fmt::write(&mut prefix, format_args!("{object:?}"));
        prefix.value == expected
    })
}

fn filter_argument(
    filter: &str,
    positional: &[Value],
    index: usize,
    kwargs: &Kwargs,
    name: &str,
) -> Result<Option<Value>, Error> {
    let positional = positional.get(index).cloned();
    let keyword = if kwargs.has(name) {
        Some(kwargs.get::<Value>(name)?)
    } else {
        None
    };
    match (positional, keyword) {
        (Some(_), Some(_)) => Err(Error::new(
            ErrorKind::InvalidOperation,
            format!("{filter} got multiple values for {name}"),
        )),
        (Some(value), None) | (None, Some(value)) => Ok(Some(value)),
        (None, None) => Ok(None),
    }
}

fn join_attribute(mut item: Value, attribute: &Value) -> Result<Value, Error> {
    if attribute.is_none() {
        return Ok(item);
    }
    if let Some(path) = attribute.as_str() {
        for part in path.split('.') {
            let key = if !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()) {
                part.parse::<u64>()
                    .map(Value::from)
                    .unwrap_or_else(|_| Value::from(part))
            } else {
                Value::from(part)
            };
            item = item.get_item(&key)?;
        }
        Ok(item)
    } else {
        item.get_item(attribute)
    }
}

fn jinja_join_filter(state: &State, value: &Value, args: Rest<Value>) -> Result<Value, Error> {
    let (positional, kwargs): (&[Value], Kwargs) = from_args(&args)?;
    if positional.len() > 2 {
        return Err(Error::new(
            ErrorKind::TooManyArguments,
            "join accepts at most two positional arguments",
        ));
    }
    let joiner =
        filter_argument("join", positional, 0, &kwargs, "d")?.unwrap_or_else(|| Value::from(""));
    let attribute = filter_argument("join", positional, 1, &kwargs, "attribute")?;
    kwargs.assert_all_used()?;

    let mut items = Vec::new();
    for item in value.try_iter().map_err(|err| {
        Error::new(
            ErrorKind::InvalidOperation,
            format!("cannot join value of type {}", value.kind()),
        )
        .with_source(err)
    })? {
        if items.len() >= MAX_TEMPLATE_INTERMEDIATE_ITEMS {
            return Err(invalid_operation(
                "join intermediate items exceed the rendered XML limit",
            ));
        }
        items.push(match attribute.as_ref() {
            Some(attribute) => join_attribute(item, attribute)?,
            None => item,
        });
    }

    let joiner_text = python_display(&joiner)?;
    if state.auto_escape() == AutoEscape::Html
        && (is_markup(&joiner) || items.iter().any(is_markup))
    {
        // Markup.join escapes the delimiter and every non-Markup item, then
        // returns Markup.  Reproduce that explicitly while retaining safe
        // rich values unchanged.
        let separator = if is_markup(&joiner) {
            joiner_text
        } else {
            jinja_html_escape(&joiner_text)?
        };
        let mut rendered = String::new();
        for (index, item) in items.iter().enumerate() {
            if index > 0 {
                push_filter_output(&mut rendered, &separator, "join", MAX_RENDERED_XML_BYTES)?;
            }
            let item = python_display(item)?;
            if is_markup(&items[index]) {
                push_filter_output(&mut rendered, &item, "join", MAX_RENDERED_XML_BYTES)?;
            } else {
                let escaped = jinja_html_escape(&item)?;
                push_filter_output(&mut rendered, &escaped, "join", MAX_RENDERED_XML_BYTES)?;
            }
        }
        return Ok(Value::from_safe_string(rendered));
    }

    let mut rendered = String::new();
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            push_filter_output(&mut rendered, &joiner_text, "join", MAX_RENDERED_XML_BYTES)?;
        }
        push_filter_output(
            &mut rendered,
            &python_display(item)?,
            "join",
            MAX_RENDERED_XML_BYTES,
        )?;
    }
    Ok(Value::from(rendered))
}

fn replace_count(value: Option<&Value>) -> Result<Option<usize>, Error> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_none() {
        return Ok(None);
    }
    if value.kind() == ValueKind::Bool {
        return Ok(Some(usize::from(bool::try_from(value.clone())?)));
    }
    if !value.is_integer() {
        return Err(Error::new(
            ErrorKind::InvalidOperation,
            "replace count must be an integer or none",
        ));
    }
    if let Ok(signed) = i64::try_from(value.clone()) {
        return if signed < 0 {
            Ok(None)
        } else {
            Ok(Some(signed as usize))
        };
    }
    usize::try_from(value.clone()).map(Some).map_err(|_| {
        Error::new(
            ErrorKind::InvalidOperation,
            "replace count is outside the supported range",
        )
    })
}

fn push_filter_output(
    output: &mut String,
    value: &str,
    filter: &str,
    max: usize,
) -> Result<(), Error> {
    if value.len() > max.saturating_sub(output.len()) {
        return Err(Error::new(
            ErrorKind::InvalidOperation,
            format!("{filter} output exceeds the rendered XML limit"),
        ));
    }
    output.push_str(value);
    Ok(())
}

/// Python `str.replace` with a hard output bound.  Iterating match positions
/// avoids allocating the full multiplicatively-expanded result before the
/// render writer can enforce its own 600 MiB limit.  `match_indices("")`
/// yields every UTF-8 character boundary, matching Python's empty-pattern
/// insertion semantics.
fn replace_text_limited(
    value: &str,
    old: &str,
    new: &str,
    count: Option<usize>,
    max: usize,
) -> Result<String, Error> {
    let mut rendered = String::with_capacity(value.len().min(max));
    let mut cursor = 0;
    let limit = count.unwrap_or(usize::MAX);
    for (index, _) in value.match_indices(old).take(limit) {
        push_filter_output(&mut rendered, &value[cursor..index], "replace", max)?;
        push_filter_output(&mut rendered, new, "replace", max)?;
        cursor = index + old.len();
    }
    push_filter_output(&mut rendered, &value[cursor..], "replace", max)?;
    Ok(rendered)
}

fn jinja_replace_filter(state: &State, value: &Value, args: Rest<Value>) -> Result<Value, Error> {
    let (positional, kwargs): (&[Value], Kwargs) = from_args(&args)?;
    if positional.len() > 3 {
        return Err(Error::new(
            ErrorKind::TooManyArguments,
            "replace accepts at most three positional arguments",
        ));
    }
    let old = filter_argument("replace", positional, 0, &kwargs, "old")?
        .ok_or_else(|| Error::new(ErrorKind::MissingArgument, "replace requires an old value"))?;
    let new = filter_argument("replace", positional, 1, &kwargs, "new")?
        .ok_or_else(|| Error::new(ErrorKind::MissingArgument, "replace requires a new value"))?;
    let count = filter_argument("replace", positional, 2, &kwargs, "count")?;
    kwargs.assert_all_used()?;
    let count = replace_count(count.as_ref())?;

    let value_text = python_display(value)?;
    let old_text = python_display(&old)?;
    let new_text = python_display(&new)?;
    let value_has_html = is_markup(value);
    let escape_source = is_markup(&old) || (is_markup(&new) && !value_has_html);
    if state.auto_escape() == AutoEscape::Html && (escape_source || value.is_safe()) {
        let value_text = if value_has_html {
            value_text
        } else {
            jinja_html_escape(&value_text)?
        };
        // Markup.replace searches for `old` verbatim.  Only the replacement
        // value is escaped before insertion into a safe result.
        let new_text = if new.is_safe() {
            new_text
        } else {
            jinja_html_escape(&new_text)?
        };
        Ok(Value::from_safe_string(replace_text_limited(
            &value_text,
            &old_text,
            &new_text,
            count,
            MAX_RENDERED_XML_BYTES,
        )?))
    } else {
        Ok(Value::from(replace_text_limited(
            &value_text,
            &old_text,
            &new_text,
            count,
            MAX_RENDERED_XML_BYTES,
        )?))
    }
}

/// Account for the conservative upper bound used before MiniJinja's printf
/// formatter is allowed to allocate its result.
fn add_format_budget(estimated: &mut usize, amount: usize, kind: &str) -> Result<(), Error> {
    *estimated = estimated.checked_add(amount).ok_or_else(|| {
        invalid_operation(format!("format {kind} exceeds the rendered XML limit"))
    })?;
    if *estimated > MAX_RENDERED_XML_BYTES {
        return Err(invalid_operation(format!(
            "format {kind} exceeds the rendered XML limit"
        )));
    }
    Ok(())
}

fn dynamic_format_budget(positional: &[Value], index: usize) -> usize {
    positional
        .get(index)
        .and_then(|value| i64::try_from(value.clone()).ok())
        .map_or(0, |value| {
            usize::try_from(value.unsigned_abs()).unwrap_or(usize::MAX)
        })
}

fn format_value_budget(value: &Value) -> Result<usize, Error> {
    Ok(match value.kind() {
        ValueKind::String => value.as_str().unwrap_or_default().len(),
        ValueKind::Bytes => value.as_bytes().unwrap_or_default().len(),
        ValueKind::Number => python_display(value)?
            .len()
            .saturating_mul(2)
            .saturating_add(32),
        _ => python_display(value)?.len(),
    })
}

fn validate_printf_format(
    format: &str,
    positional: &[Value],
    kwargs: &Kwargs,
) -> Result<(), Error> {
    if format.len() > MAX_RENDERED_XML_BYTES {
        return Err(invalid_operation(
            "format input exceeds the rendered XML limit",
        ));
    }

    let bytes = format.as_bytes();
    let mut index = 0usize;
    let mut argument_index = 0usize;
    let has_kwargs = kwargs.args().next().is_some();
    if has_kwargs && !positional.is_empty() {
        return Err(invalid_operation(
            "format filter cannot combine positional and keyword arguments",
        ));
    }
    let mut has_mapping_field = false;
    // This intentionally over-counts field syntax as literal output.  A
    // conservative upper bound is preferable to letting the underlying
    // formatter allocate beyond the 600 MiB render budget.
    let mut estimated_output = format.len();
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        index += 1;
        if bytes.get(index) == Some(&b'%') {
            index += 1;
            continue;
        }

        let is_mapping_field = bytes.get(index) == Some(&b'(');
        let mapping_value = if is_mapping_field {
            has_mapping_field = true;
            index += 1;
            let key_start = index;
            let Some(close) = bytes[index..].iter().position(|byte| *byte == b')') else {
                return Err(invalid_value("incomplete format key"));
            };
            index += close;
            let key = &format[key_start..index];
            index += 1;
            if kwargs.has(key) {
                Some(kwargs.get::<Value>(key)?)
            } else {
                None
            }
        } else {
            None
        };
        while bytes
            .get(index)
            .is_some_and(|byte| matches!(byte, b'#' | b'0' | b'-' | b' ' | b'+'))
        {
            index += 1;
        }

        if bytes.get(index) == Some(&b'*') {
            if is_mapping_field {
                return Err(invalid_operation(
                    "mapping format with dynamic width is outside the bounded compatibility surface",
                ));
            }
            add_format_budget(
                &mut estimated_output,
                dynamic_format_budget(positional, argument_index),
                "width",
            )?;
            argument_index = argument_index.saturating_add(1);
            index += 1;
        } else {
            let mut width = 0usize;
            while let Some(byte) = bytes.get(index).filter(|byte| byte.is_ascii_digit()) {
                width = width
                    .saturating_mul(10)
                    .saturating_add(usize::from(*byte - b'0'));
                index += 1;
            }
            add_format_budget(&mut estimated_output, width, "width")?;
        }

        if bytes.get(index) == Some(&b'.') {
            index += 1;
            if bytes.get(index) == Some(&b'*') {
                if is_mapping_field {
                    return Err(invalid_operation(
                        "mapping format with dynamic precision is outside the bounded compatibility surface",
                    ));
                }
                add_format_budget(
                    &mut estimated_output,
                    dynamic_format_budget(positional, argument_index),
                    "precision",
                )?;
                argument_index = argument_index.saturating_add(1);
                index += 1;
            } else {
                let mut precision = 0usize;
                while let Some(byte) = bytes.get(index).filter(|byte| byte.is_ascii_digit()) {
                    precision = precision
                        .saturating_mul(10)
                        .saturating_add(usize::from(*byte - b'0'));
                    index += 1;
                }
                add_format_budget(&mut estimated_output, precision, "precision")?;
            }
        }
        if bytes
            .get(index)
            .is_some_and(|byte| matches!(byte, b'h' | b'l' | b'L'))
        {
            index += 1;
        }
        let conversion = *bytes
            .get(index)
            .ok_or_else(|| invalid_value("incomplete format"))?;
        if !matches!(
            conversion,
            b'd' | b'i'
                | b'o'
                | b'u'
                | b'x'
                | b'X'
                | b'e'
                | b'E'
                | b'f'
                | b'F'
                | b'g'
                | b'G'
                | b'c'
                | b'r'
                | b's'
                | b'a'
        ) {
            return Err(invalid_value(format!(
                "unsupported format character {:?}",
                char::from(conversion)
            )));
        }
        if matches!(conversion, b'a' | b'r' | b'u') {
            return Err(invalid_operation(
                "printf a/r/u conversions remain outside the bounded compatibility surface",
            ));
        }
        if is_mapping_field {
            if let Some(value) = mapping_value.as_ref() {
                add_format_budget(&mut estimated_output, format_value_budget(value)?, "output")?;
            }
        } else {
            argument_index = argument_index.saturating_add(1);
        }
        index += 1;
    }

    for value in positional {
        add_format_budget(&mut estimated_output, format_value_budget(value)?, "output")?;
    }
    if !has_mapping_field && argument_index != positional.len() {
        return Err(invalid_operation(
            "format filter did not consume exactly all positional arguments",
        ));
    }
    Ok(())
}

fn positional_printf_conversions(format: &str) -> Result<Option<Vec<u8>>, Error> {
    let bytes = format.as_bytes();
    let mut conversions = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        index += 1;
        if bytes.get(index) == Some(&b'%') {
            index += 1;
            continue;
        }
        if bytes.get(index) == Some(&b'(') {
            return Ok(None);
        }
        while bytes
            .get(index)
            .is_some_and(|byte| matches!(byte, b'#' | b'0' | b'-' | b' ' | b'+'))
        {
            index += 1;
        }
        if bytes.get(index) == Some(&b'*') {
            return Ok(None);
        }
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if bytes.get(index) == Some(&b'.') {
            index += 1;
            if bytes.get(index) == Some(&b'*') {
                return Ok(None);
            }
            while bytes.get(index).is_some_and(u8::is_ascii_digit) {
                index += 1;
            }
        }
        if bytes
            .get(index)
            .is_some_and(|byte| matches!(byte, b'h' | b'l' | b'L'))
        {
            index += 1;
        }
        let conversion = *bytes
            .get(index)
            .ok_or_else(|| invalid_value("incomplete format"))?;
        if !matches!(
            conversion,
            b'd' | b'i'
                | b'o'
                | b'x'
                | b'X'
                | b'e'
                | b'E'
                | b'f'
                | b'F'
                | b'g'
                | b'G'
                | b'c'
                | b's'
        ) {
            // Python accepts these conversions, but they remain on the
            // MiniJinja fallback path because preserving their exact repr
            // semantics requires the original Python value type (DEV-0015).
            if matches!(conversion, b'a' | b'r' | b'u') {
                return Ok(None);
            }
            return Err(invalid_value(format!(
                "unsupported format character {:?}",
                char::from(conversion)
            )));
        }
        conversions.push(conversion);
        index += 1;
    }
    Ok(Some(conversions))
}

fn jinja_format_filter(
    state: &State,
    format_value: &Value,
    args: Rest<Value>,
) -> Result<Value, Error> {
    let Some(format) = format_value.as_str() else {
        return Err(Error::new(
            ErrorKind::InvalidOperation,
            "value is not a string",
        ));
    };
    let (positional, kwargs): (&[Value], Kwargs) = from_args(&args)?;
    validate_printf_format(format, positional, &kwargs)?;
    let conversions = positional_printf_conversions(format)?;
    if kwargs.args().next().is_none() {
        if let Some(conversions) = conversions {
            let mut transformed = positional.to_vec();
            for (value, conversion) in transformed.iter_mut().zip(conversions) {
                if conversion == b's' && !value.is_safe() {
                    *value = Value::from(python_display(value)?);
                }
            }
            return minijinja::filters::format(state, format_value, Rest(transformed));
        }
    }
    minijinja::filters::format(state, format_value, args)
}

fn jinja_xmlattr(state: &State, value: &Value, args: Rest<Value>) -> Result<Value, Error> {
    let (positional, kwargs): (&[Value], Kwargs) = from_args(&args)?;
    if positional.len() > 1 {
        return Err(Error::new(
            ErrorKind::TooManyArguments,
            "xmlattr accepts at most one positional argument",
        ));
    }
    let keyword_autospace = if kwargs.has("autospace") {
        Some(kwargs.get::<Value>("autospace")?)
    } else {
        None
    };
    kwargs.assert_all_used()?;
    let autospace = match (positional.first(), keyword_autospace.as_ref()) {
        (Some(_), Some(_)) => {
            return Err(Error::new(
                ErrorKind::InvalidOperation,
                "xmlattr got multiple values for autospace",
            ));
        }
        (Some(value), None) | (None, Some(value)) => value.is_true(),
        (None, None) => true,
    };
    if value.kind() != ValueKind::Map {
        return Err(Error::new(
            ErrorKind::InvalidOperation,
            "xmlattr requires a mapping",
        ));
    }
    let mut rendered = String::new();
    for key in value.try_iter()? {
        let Some(key) = key.as_str() else {
            return Err(Error::new(
                ErrorKind::InvalidOperation,
                "xmlattr attribute names must be strings",
            ));
        };
        if key
            .chars()
            .any(|ch| ch.is_ascii_whitespace() || ch == '\u{b}' || matches!(ch, '/' | '>' | '='))
        {
            return Err(invalid_value(format!(
                "Invalid character in attribute name: {key:?}"
            )));
        }
        let item = value.get_item(&Value::from(key))?;
        if item.is_none() || item.is_undefined() {
            continue;
        }
        let escaped_value = if let Some(markup) = rich_markup(&item) {
            markup.to_owned()
        } else if item.is_safe() {
            item.as_str().unwrap_or_default().to_owned()
        } else {
            jinja_html_escape(&python_display(&item)?)?
        };
        if rendered.is_empty() {
            if autospace {
                push_filter_output(&mut rendered, " ", "xmlattr", MAX_RENDERED_XML_BYTES)?;
            }
        } else {
            push_filter_output(&mut rendered, " ", "xmlattr", MAX_RENDERED_XML_BYTES)?;
        }
        let escaped_key = jinja_html_escape(key)?;
        for fragment in [escaped_key.as_str(), "=\"", escaped_value.as_str(), "\""] {
            push_filter_output(&mut rendered, fragment, "xmlattr", MAX_RENDERED_XML_BYTES)?;
        }
    }
    if state.auto_escape() == AutoEscape::Html {
        Ok(Value::from_safe_string(rendered))
    } else {
        Ok(Value::from(rendered))
    }
}

fn jinja_html_escape(value: &str) -> Result<String, Error> {
    let mut escaped = String::with_capacity(value.len().min(MAX_RENDERED_XML_BYTES));
    for ch in value.chars() {
        match ch {
            '&' => push_filter_output(&mut escaped, "&amp;", "escape", MAX_RENDERED_XML_BYTES)?,
            '<' => push_filter_output(&mut escaped, "&lt;", "escape", MAX_RENDERED_XML_BYTES)?,
            '>' => push_filter_output(&mut escaped, "&gt;", "escape", MAX_RENDERED_XML_BYTES)?,
            '\'' => push_filter_output(&mut escaped, "&#39;", "escape", MAX_RENDERED_XML_BYTES)?,
            '"' => push_filter_output(&mut escaped, "&#34;", "escape", MAX_RENDERED_XML_BYTES)?,
            other => {
                let mut encoded = [0; 4];
                push_filter_output(
                    &mut escaped,
                    other.encode_utf8(&mut encoded),
                    "escape",
                    MAX_RENDERED_XML_BYTES,
                )?;
            }
        }
    }
    Ok(escaped)
}

fn jinja_tojson(value: &Value, args: Rest<Value>) -> Result<Value, Error> {
    let (positional, kwargs): (&[Value], Kwargs) = from_args(&args)?;
    if positional.len() > 1 {
        return Err(Error::new(
            ErrorKind::TooManyArguments,
            "tojson accepts at most one positional argument",
        ));
    }
    let keyword_indent = if kwargs.has("indent") {
        Some(kwargs.get::<Value>("indent")?)
    } else {
        None
    };
    let indent = match (positional.first(), keyword_indent) {
        (Some(_), Some(_)) => {
            return Err(Error::new(
                ErrorKind::InvalidOperation,
                "tojson got multiple values for indent",
            ));
        }
        (Some(value), None) => Some((*value).clone()),
        (None, value) => value,
    };
    kwargs.assert_all_used()?;
    let indent = match indent {
        None => None,
        Some(value) if value.is_none() => None,
        Some(value) if value.is_undefined() => {
            return Err(Error::new(
                ErrorKind::UndefinedError,
                "tojson indent is undefined",
            ));
        }
        Some(value) if value.kind() == ValueKind::Bool => {
            Some(JsonIndent::Spaces(usize::from(bool::try_from(value)?)))
        }
        Some(value) if value.is_integer() => {
            let width = i64::try_from(value)?.max(0) as usize;
            if width > MAX_RENDERED_XML_BYTES {
                return Err(Error::new(
                    ErrorKind::InvalidOperation,
                    "tojson indent exceeds the rendered XML limit",
                ));
            }
            Some(JsonIndent::Spaces(width))
        }
        Some(value) if value.kind() == ValueKind::String => {
            let text = value.as_str().unwrap_or_default().to_owned();
            if text.len() > MAX_RENDERED_XML_BYTES {
                return Err(Error::new(
                    ErrorKind::InvalidOperation,
                    "tojson indent exceeds the rendered XML limit",
                ));
            }
            Some(JsonIndent::Text(text))
        }
        Some(_) => {
            return Err(Error::new(
                ErrorKind::InvalidOperation,
                "tojson indent must be an integer or string",
            ));
        }
    };

    let json = python_json_value(value)?;
    let mut rendered = JsonOutput::default();
    write_python_json(&json, indent.as_ref(), 0, &mut rendered)?;
    Ok(Value::from_safe_string(htmlsafe_ascii_json(
        rendered.as_str(),
    )?))
}

#[derive(Debug, Clone)]
enum JsonIndent {
    Spaces(usize),
    Text(String),
}

#[derive(Debug, Default)]
struct JsonOutput {
    value: String,
}

impl JsonOutput {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            value: String::with_capacity(capacity.min(MAX_RENDERED_XML_BYTES)),
        }
    }

    fn push(&mut self, ch: char) -> Result<(), Error> {
        let mut encoded = [0; 4];
        self.push_str(ch.encode_utf8(&mut encoded))
    }

    fn push_str(&mut self, value: &str) -> Result<(), Error> {
        if value.len() > MAX_RENDERED_XML_BYTES.saturating_sub(self.value.len()) {
            return Err(Error::new(
                ErrorKind::InvalidOperation,
                "tojson output exceeds the rendered XML limit",
            ));
        }
        self.value.push_str(value);
        Ok(())
    }

    fn as_str(&self) -> &str {
        &self.value
    }

    fn finish(self) -> String {
        self.value
    }
}

impl JsonIndent {
    fn padding(&self, level: usize) -> Result<String, Error> {
        let unit_len = match self {
            Self::Spaces(width) => *width,
            Self::Text(text) => text.len(),
        };
        let len = unit_len.checked_mul(level).ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidOperation,
                "tojson indentation exceeds the rendered XML limit",
            )
        })?;
        if len > MAX_RENDERED_XML_BYTES {
            return Err(Error::new(
                ErrorKind::InvalidOperation,
                "tojson indentation exceeds the rendered XML limit",
            ));
        }
        Ok(match self {
            Self::Spaces(width) => " ".repeat(level * *width),
            Self::Text(text) => text.repeat(level),
        })
    }
}

#[derive(Debug, Clone)]
enum PythonJsonValue {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<PythonJsonValue>),
    Object(Vec<(String, PythonJsonValue)>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JsonKeyKind {
    String,
    Number,
    None,
}

/// Convert only the value categories accepted by Python's json.dumps.  This
/// deliberately rejects MiniJinja iterables/functions/bytes and Undefined;
/// serde_json's generic Value bridge would otherwise over-serialize them or
/// silently turn Undefined and non-finite floats into null.
fn python_json_value(value: &Value) -> Result<PythonJsonValue, Error> {
    match value.kind() {
        ValueKind::Undefined => Err(json_type_error("Undefined")),
        ValueKind::None => Ok(PythonJsonValue::Null),
        ValueKind::Bool => Ok(PythonJsonValue::Bool(bool::try_from(value.clone())?)),
        ValueKind::Number => Ok(PythonJsonValue::Number(python_json_number_value(value)?)),
        ValueKind::String => Ok(PythonJsonValue::String(
            value.as_str().unwrap_or_default().to_owned(),
        )),
        ValueKind::Seq => {
            let mut items = Vec::new();
            for item in value.try_iter()? {
                items.push(python_json_value(&item)?);
            }
            Ok(PythonJsonValue::Array(items))
        }
        ValueKind::Map => {
            let mut entries = Vec::new();
            for key in value.try_iter()? {
                let (kind, encoded_key) = python_json_key(&key)?;
                let converted_value = python_json_value(&value.get_item(&key)?)?;
                // Python bool is a subclass of int, so True/1 and False/0
                // compare equal as dict keys.  MiniJinja keeps them as
                // distinct key kinds; normalize bool only for JSON-key
                // equality/sorting, while retaining the first key's spelling
                // and replacing its value like a Python dict assignment.
                let sort_key = if key.kind() == ValueKind::Bool {
                    Value::from(i64::from(bool::try_from(key.clone())?))
                } else {
                    key.clone()
                };
                if kind == JsonKeyKind::Number {
                    if let Some(existing) =
                        entries
                            .iter_mut()
                            .find(|(existing_kind, existing_key, _, _)| {
                                *existing_kind == JsonKeyKind::Number && *existing_key == sort_key
                            })
                    {
                        existing.3 = converted_value;
                        continue;
                    }
                }
                entries.push((kind, sort_key, encoded_key, converted_value));
            }
            if let Some(first) = entries.first().map(|entry| entry.0) {
                if entries.iter().any(|entry| entry.0 != first) {
                    return Err(Error::new(
                        ErrorKind::InvalidOperation,
                        "cannot sort JSON object keys of different Python types",
                    ));
                }
                match first {
                    JsonKeyKind::String => entries.sort_by(|left, right| left.2.cmp(&right.2)),
                    JsonKeyKind::Number => entries.sort_by(|left, right| left.1.cmp(&right.1)),
                    JsonKeyKind::None => {}
                }
            }
            Ok(PythonJsonValue::Object(
                entries
                    .into_iter()
                    .map(|(_, _, key, value)| (key, value))
                    .collect(),
            ))
        }
        ValueKind::Bytes => Err(json_type_error("bytes")),
        ValueKind::Iterable => Err(json_type_error("iterable")),
        ValueKind::Plain => Err(json_type_error("object")),
        ValueKind::Invalid => Err(json_type_error("invalid value")),
        _ => Err(json_type_error("object")),
    }
}

fn python_json_key(value: &Value) -> Result<(JsonKeyKind, String), Error> {
    match value.kind() {
        ValueKind::String => Ok((
            JsonKeyKind::String,
            value.as_str().unwrap_or_default().to_owned(),
        )),
        ValueKind::None => Ok((JsonKeyKind::None, "null".to_string())),
        ValueKind::Bool => Ok((
            JsonKeyKind::Number,
            if bool::try_from(value.clone())? {
                "true".to_string()
            } else {
                "false".to_string()
            },
        )),
        ValueKind::Number => Ok((JsonKeyKind::Number, python_json_number_value(value)?)),
        _ => Err(Error::new(
            ErrorKind::InvalidOperation,
            "JSON object keys must be str, int, float, bool or None",
        )),
    }
}

fn json_type_error(type_name: &str) -> Error {
    Error::new(
        ErrorKind::InvalidOperation,
        format!("Object of type {type_name} is not JSON serializable"),
    )
}

fn write_python_json(
    value: &PythonJsonValue,
    indent: Option<&JsonIndent>,
    level: usize,
    out: &mut JsonOutput,
) -> Result<(), Error> {
    match value {
        PythonJsonValue::Null => out.push_str("null")?,
        PythonJsonValue::Bool(value) => out.push_str(if *value { "true" } else { "false" })?,
        PythonJsonValue::Number(value) => out.push_str(value)?,
        PythonJsonValue::String(value) => write_json_string(value, out)?,
        PythonJsonValue::Array(items) => {
            out.push('[')?;
            write_json_items(items.iter(), indent, level, out, write_python_json)?;
            out.push(']')?;
        }
        PythonJsonValue::Object(entries) => {
            out.push('{')?;
            write_json_items(
                entries.iter(),
                indent,
                level,
                out,
                |(key, value), indent, level, out| {
                    write_json_string(key, out)?;
                    out.push_str(": ")?;
                    write_python_json(value, indent, level, out)
                },
            )?;
            out.push('}')?;
        }
    }
    Ok(())
}

fn write_json_string(value: &str, out: &mut JsonOutput) -> Result<(), Error> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push('"')?;
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\"")?,
            '\\' => out.push_str("\\\\")?,
            '\u{08}' => out.push_str("\\b")?,
            '\t' => out.push_str("\\t")?,
            '\n' => out.push_str("\\n")?,
            '\u{0c}' => out.push_str("\\f")?,
            '\r' => out.push_str("\\r")?,
            control if control <= '\u{1f}' => {
                let byte = control as u8;
                out.push('\\')?;
                out.push('u')?;
                out.push('0')?;
                out.push('0')?;
                out.push(char::from(HEX[(byte >> 4) as usize]))?;
                out.push(char::from(HEX[(byte & 0x0f) as usize]))?;
            }
            other => out.push(other)?,
        }
    }
    out.push('"')
}

fn write_json_items<I, T, F>(
    items: I,
    indent: Option<&JsonIndent>,
    level: usize,
    out: &mut JsonOutput,
    mut write_item: F,
) -> Result<(), Error>
where
    I: IntoIterator<Item = T>,
    F: FnMut(T, Option<&JsonIndent>, usize, &mut JsonOutput) -> Result<(), Error>,
{
    let items: Vec<T> = items.into_iter().collect();
    if items.is_empty() {
        return Ok(());
    }
    match indent {
        None => {
            for (index, item) in items.into_iter().enumerate() {
                if index > 0 {
                    out.push_str(", ")?;
                }
                write_item(item, None, level + 1, out)?;
            }
        }
        Some(indent) => {
            out.push('\n')?;
            let child_padding = indent.padding(level + 1)?;
            for (index, item) in items.into_iter().enumerate() {
                if index > 0 {
                    out.push_str(",\n")?;
                }
                out.push_str(&child_padding)?;
                write_item(item, Some(indent), level + 1, out)?;
            }
            out.push('\n')?;
            out.push_str(&indent.padding(level)?)?;
        }
    }
    Ok(())
}

fn python_json_number_value(value: &Value) -> Result<String, Error> {
    if value.is_integer() {
        return Ok(value.to_string());
    }
    Ok(python_float_display(f64::try_from(value.clone())?, true))
}

/// Python switches finite float repr to scientific notation below 1e-4 and
/// at 1e16, while ryu's JSON spelling keeps 1e-5 as `0.00001`.  Convert the
/// shortest ryu digits without changing their round-trip value, then normalize
/// the exponent sign and its minimum two-digit width.
fn python_float_display(value: f64, json_mode: bool) -> String {
    if value.is_nan() {
        return if json_mode { "NaN" } else { "nan" }.to_string();
    }
    if value.is_infinite() {
        return match (json_mode, value.is_sign_negative()) {
            (true, true) => "-Infinity",
            (true, false) => "Infinity",
            (false, true) => "-inf",
            (false, false) => "inf",
        }
        .to_string();
    }
    let raw = serde_json::Number::from_f64(value)
        .map_or_else(|| value.to_string(), |number| number.to_string());
    python_finite_float(&raw)
}

fn python_finite_float(raw: &str) -> String {
    if let Some(index) = raw.find(['e', 'E']) {
        return normalize_python_exponent(&raw[..index], &raw[index + 1..]);
    }

    let unsigned = raw.strip_prefix('-').unwrap_or(raw);
    let sign = if raw.starts_with('-') { "-" } else { "" };
    let (integer, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    let nonzero_integer = integer.find(|ch| ch != '0');
    let exponent = if let Some(index) = nonzero_integer {
        (integer.len() - index - 1) as i32
    } else if let Some(index) = fraction.find(|ch| ch != '0') {
        -(index as i32) - 1
    } else {
        return raw.to_owned();
    };

    if (-4..16).contains(&exponent) {
        return raw.to_owned();
    }

    let mut digits: String = integer
        .chars()
        .chain(fraction.chars())
        .skip_while(|ch| *ch == '0')
        .collect();
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
    }
    let mut mantissa = String::new();
    mantissa.push_str(sign);
    if let Some(first) = digits.chars().next() {
        mantissa.push(first);
        if digits.len() > first.len_utf8() {
            mantissa.push('.');
            mantissa.push_str(&digits[first.len_utf8()..]);
        }
    }
    normalize_python_exponent(&mantissa, &exponent.to_string())
}

fn normalize_python_exponent(mantissa: &str, exponent: &str) -> String {
    let (sign, digits) = match exponent.as_bytes().first() {
        Some(b'+') => ('+', &exponent[1..]),
        Some(b'-') => ('-', &exponent[1..]),
        _ => ('+', exponent),
    };
    let width = digits.len().max(2);
    format!("{mantissa}e{sign}{digits:0>width$}")
}

fn htmlsafe_ascii_json(serialized: &str) -> Result<String, Error> {
    let mut escaped = JsonOutput::with_capacity(serialized.len());
    for ch in serialized.chars() {
        match ch {
            '<' => escaped.push_str("\\u003c")?,
            '>' => escaped.push_str("\\u003e")?,
            '&' => escaped.push_str("\\u0026")?,
            '\'' => escaped.push_str("\\u0027")?,
            '\u{7f}' => escaped.push_str("\\u007f")?,
            ch if !ch.is_ascii() => {
                let scalar = ch as u32;
                if scalar <= 0xffff {
                    escaped.push_str(&format!("\\u{scalar:04x}"))?;
                } else {
                    let scalar = scalar - 0x1_0000;
                    let high = 0xd800 + (scalar >> 10);
                    let low = 0xdc00 + (scalar & 0x3ff);
                    escaped.push_str(&format!("\\u{high:04x}\\u{low:04x}"))?;
                }
            }
            other => escaped.push(other)?,
        }
    }
    Ok(escaped.finish())
}

fn jinja_urlencode(value: &Value) -> Result<String, Error> {
    if value.kind() == ValueKind::Map {
        let mut pairs = Vec::new();
        for key in value.try_iter()? {
            if pairs.len() >= MAX_TEMPLATE_INTERMEDIATE_ITEMS {
                return Err(invalid_operation(
                    "urlencode intermediate items exceed the rendered XML limit",
                ));
            }
            pairs.push((key.clone(), value.get_item(&key)?));
        }
        return encode_query_pairs(pairs);
    }
    // Jinja's filter treats every non-string iterable as a sequence of
    // key/value pairs (for example `range(0)` is an empty query).  MiniJinja
    // ranges are not ValueKind::Seq, so use the iteration capability rather
    // than the concrete value kind.
    if !matches!(
        value.kind(),
        ValueKind::String
            | ValueKind::Bytes
            | ValueKind::None
            | ValueKind::Undefined
            | ValueKind::Number
            | ValueKind::Bool
    ) {
        if let Ok(iter) = value.try_iter() {
            let mut pairs = Vec::new();
            for item in iter {
                if pairs.len() >= MAX_TEMPLATE_INTERMEDIATE_ITEMS {
                    return Err(invalid_operation(
                        "urlencode intermediate items exceed the rendered XML limit",
                    ));
                }
                let mut pair = item.try_iter()?;
                let pair = match (pair.next(), pair.next(), pair.next()) {
                    (Some(first), Some(second), None) => (first, second),
                    _ => {
                        return Err(invalid_value(
                            "urlencode sequence items must contain exactly two values",
                        ));
                    }
                };
                pairs.push(pair);
            }
            return encode_query_pairs(pairs);
        }
    }

    if let Some(bytes) = value.as_bytes() {
        return percent_encode_limited(bytes, false);
    }
    percent_encode_limited(python_display(value)?.as_bytes(), false)
}

fn encode_query_pairs(pairs: Vec<(Value, Value)>) -> Result<String, Error> {
    let mut rendered = String::new();
    for (index, (key, value)) in pairs.into_iter().enumerate() {
        if index > 0 {
            push_filter_output(&mut rendered, "&", "urlencode", MAX_RENDERED_XML_BYTES)?;
        }
        push_percent_encoded(
            &mut rendered,
            python_display(&key)?.as_bytes(),
            true,
            MAX_RENDERED_XML_BYTES,
        )?;
        push_filter_output(&mut rendered, "=", "urlencode", MAX_RENDERED_XML_BYTES)?;
        push_percent_encoded(
            &mut rendered,
            python_display(&value)?.as_bytes(),
            true,
            MAX_RENDERED_XML_BYTES,
        )?;
    }
    Ok(rendered)
}

/// Python/Jinja string conversion for values originating from JSON/context.
/// MiniJinja intentionally formats sequences and maps with JSON-style double
/// quoted strings and formats floats with Rust's thresholds; Jinja2 uses
/// Python `str`, whose containers recursively use Python `repr`.
fn python_display(value: &Value) -> Result<String, Error> {
    match value.kind() {
        ValueKind::Undefined => Ok(String::new()),
        ValueKind::None => Ok("None".to_string()),
        ValueKind::Bool => Ok(if bool::try_from(value.clone())? {
            "True".to_string()
        } else {
            "False".to_string()
        }),
        ValueKind::Number => python_number_display(value),
        ValueKind::String => Ok(value.as_str().unwrap_or_default().to_owned()),
        ValueKind::Bytes => {
            Ok(String::from_utf8_lossy(value.as_bytes().unwrap_or_default()).into())
        }
        ValueKind::Seq | ValueKind::Map => python_repr(value),
        _ => Ok(value.to_string()),
    }
}

fn python_repr(value: &Value) -> Result<String, Error> {
    let mut rendered = String::new();
    push_python_repr(&mut rendered, value)?;
    Ok(rendered)
}

fn push_python_repr(rendered: &mut String, value: &Value) -> Result<(), Error> {
    match value.kind() {
        ValueKind::Undefined => push_filter_output(
            rendered,
            "Undefined",
            "string conversion",
            MAX_RENDERED_XML_BYTES,
        ),
        ValueKind::None | ValueKind::Bool | ValueKind::Number => push_filter_output(
            rendered,
            &python_display(value)?,
            "string conversion",
            MAX_RENDERED_XML_BYTES,
        ),
        ValueKind::String => push_python_string_repr(rendered, value.as_str().unwrap_or_default()),
        ValueKind::Bytes => push_python_bytes_repr(rendered, value.as_bytes().unwrap_or_default()),
        ValueKind::Seq => {
            push_filter_output(rendered, "[", "string conversion", MAX_RENDERED_XML_BYTES)?;
            for (index, item) in value.try_iter()?.enumerate() {
                if index > 0 {
                    push_filter_output(
                        rendered,
                        ", ",
                        "string conversion",
                        MAX_RENDERED_XML_BYTES,
                    )?;
                }
                push_python_repr(rendered, &item)?;
            }
            push_filter_output(rendered, "]", "string conversion", MAX_RENDERED_XML_BYTES)
        }
        ValueKind::Map => {
            push_filter_output(rendered, "{", "string conversion", MAX_RENDERED_XML_BYTES)?;
            for (index, key) in value.try_iter()?.enumerate() {
                if index > 0 {
                    push_filter_output(
                        rendered,
                        ", ",
                        "string conversion",
                        MAX_RENDERED_XML_BYTES,
                    )?;
                }
                push_python_repr(rendered, &key)?;
                push_filter_output(rendered, ": ", "string conversion", MAX_RENDERED_XML_BYTES)?;
                push_python_repr(rendered, &value.get_item(&key)?)?;
            }
            push_filter_output(rendered, "}", "string conversion", MAX_RENDERED_XML_BYTES)
        }
        _ => push_filter_output(
            rendered,
            &value.to_string(),
            "string conversion",
            MAX_RENDERED_XML_BYTES,
        ),
    }
}

fn python_number_display(value: &Value) -> Result<String, Error> {
    if value.is_integer() {
        return Ok(value.to_string());
    }
    Ok(python_float_display(f64::try_from(value.clone())?, false))
}

#[cfg(test)]
fn python_string_repr(value: &str) -> Result<String, Error> {
    let mut rendered = String::with_capacity(value.len().min(MAX_RENDERED_XML_BYTES));
    push_python_string_repr(&mut rendered, value)?;
    Ok(rendered)
}

fn push_python_string_repr(rendered: &mut String, value: &str) -> Result<(), Error> {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut quote_buffer = [0; 4];
    let quote_text = quote.encode_utf8(&mut quote_buffer);
    push_filter_output(
        rendered,
        quote_text,
        "string conversion",
        MAX_RENDERED_XML_BYTES,
    )?;
    for ch in value.chars() {
        match ch {
            '\\' => push_filter_output(
                rendered,
                "\\\\",
                "string conversion",
                MAX_RENDERED_XML_BYTES,
            )?,
            '\t' => {
                push_filter_output(rendered, "\\t", "string conversion", MAX_RENDERED_XML_BYTES)?
            }
            '\n' => {
                push_filter_output(rendered, "\\n", "string conversion", MAX_RENDERED_XML_BYTES)?
            }
            '\r' => {
                push_filter_output(rendered, "\\r", "string conversion", MAX_RENDERED_XML_BYTES)?
            }
            ch if ch == quote => push_filter_output(
                rendered,
                if quote == '\'' { "\\'" } else { "\\\"" },
                "string conversion",
                MAX_RENDERED_XML_BYTES,
            )?,
            ch if ch <= '\u{1f}' || ch == '\u{7f}' => push_filter_output(
                rendered,
                &format!("\\x{:02x}", ch as u32),
                "string conversion",
                MAX_RENDERED_XML_BYTES,
            )?,
            ch if ch > '\u{7f}' && ch.escape_debug().to_string() != ch.to_string() => {
                let escaped = match ch as u32 {
                    scalar @ 0x80..=0xff => format!("\\x{scalar:02x}"),
                    scalar @ 0x100..=0xffff => format!("\\u{scalar:04x}"),
                    scalar => format!("\\U{scalar:08x}"),
                };
                push_filter_output(
                    rendered,
                    &escaped,
                    "string conversion",
                    MAX_RENDERED_XML_BYTES,
                )?;
            }
            ch => {
                let mut encoded = [0; 4];
                push_filter_output(
                    rendered,
                    ch.encode_utf8(&mut encoded),
                    "string conversion",
                    MAX_RENDERED_XML_BYTES,
                )?;
            }
        }
    }
    push_filter_output(
        rendered,
        quote_text,
        "string conversion",
        MAX_RENDERED_XML_BYTES,
    )
}

fn push_python_bytes_repr(rendered: &mut String, value: &[u8]) -> Result<(), Error> {
    let quote = if value.contains(&b'\'') && !value.contains(&b'"') {
        '"'
    } else {
        '\''
    };
    push_filter_output(rendered, "b", "string conversion", MAX_RENDERED_XML_BYTES)?;
    let mut quote_buffer = [0; 4];
    let quote_text = quote.encode_utf8(&mut quote_buffer);
    push_filter_output(
        rendered,
        quote_text,
        "string conversion",
        MAX_RENDERED_XML_BYTES,
    )?;
    for &byte in value {
        let escaped = match byte {
            b'\\' => "\\\\".to_string(),
            b'\t' => "\\t".to_string(),
            b'\n' => "\\n".to_string(),
            b'\r' => "\\r".to_string(),
            byte if byte == quote as u8 => if quote == '\'' { "\\'" } else { "\\\"" }.to_string(),
            0x20..=0x7e => char::from(byte).to_string(),
            byte => format!("\\x{byte:02x}"),
        };
        push_filter_output(
            rendered,
            &escaped,
            "string conversion",
            MAX_RENDERED_XML_BYTES,
        )?;
    }
    push_filter_output(
        rendered,
        quote_text,
        "string conversion",
        MAX_RENDERED_XML_BYTES,
    )
}

fn percent_encode_limited(bytes: &[u8], for_query_string: bool) -> Result<String, Error> {
    let mut rendered = String::with_capacity(bytes.len().min(MAX_RENDERED_XML_BYTES));
    push_percent_encoded(
        &mut rendered,
        bytes,
        for_query_string,
        MAX_RENDERED_XML_BYTES,
    )?;
    Ok(rendered)
}

fn push_percent_encoded(
    rendered: &mut String,
    bytes: &[u8],
    for_query_string: bool,
    max: usize,
) -> Result<(), Error> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for &byte in bytes {
        let unreserved = byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~');
        if unreserved || (!for_query_string && byte == b'/') {
            if rendered.len() >= max {
                return Err(invalid_operation(
                    "urlencode output exceeds the rendered XML limit",
                ));
            }
            rendered.push(char::from(byte));
        } else if for_query_string && byte == b' ' {
            if rendered.len() >= max {
                return Err(invalid_operation(
                    "urlencode output exceeds the rendered XML limit",
                ));
            }
            rendered.push('+');
        } else {
            if rendered.len() > max.saturating_sub(3) {
                return Err(invalid_operation(
                    "urlencode output exceeds the rendered XML limit",
                ));
            }
            rendered.push('%');
            rendered.push(char::from(HEX[(byte >> 4) as usize]));
            rendered.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
    }
    Ok(())
}

/// Renders a single template string with the given MiniJinja root value;
/// error mapping carries the part name and tag-stripped context.
#[cfg(test)]
pub(crate) fn render_inline_value(
    env: &Environment,
    template_src: &str,
    root: Value,
    part: &str,
) -> Result<String, RenderError> {
    render_inline_value_with_limit(env, template_src, root, part, MAX_RENDERED_XML_BYTES)
}

pub(crate) fn render_inline_value_with_limit(
    env: &Environment,
    template_src: &str,
    root: Value,
    part: &str,
    max: usize,
) -> Result<String, RenderError> {
    let template = env
        .template_from_str(template_src)
        .map_err(|e| map_jinja_error(&e, template_src, part))?;
    let mut output = LimitedOutput {
        bytes: Vec::new(),
        max,
        exceeded: false,
    };
    let rendered = template.render_captured_to(root, &mut output);
    if output.exceeded {
        return Err(RenderError::Limit {
            part: part.to_string(),
            kind: "rendered_xml_bytes",
            max: max as u64,
        });
    }
    if let Err(e) = rendered {
        if e.kind() == ErrorKind::OutOfFuel {
            return Err(RenderError::Limit {
                part: part.to_string(),
                kind: "template_fuel",
                max: env.fuel().unwrap_or(MAX_TEMPLATE_FUEL),
            });
        }
        return Err(map_jinja_error(&e, template_src, part));
    }
    String::from_utf8(output.bytes).map_err(|e| RenderError::Template {
        kind: TemplateErrorKind::Other,
        part: part.to_string(),
        line: None,
        message: e.to_string(),
        context: Vec::new(),
    })
}

/// Maps a MiniJinja error to a stable [`RenderError`], attaching tag-stripped
/// text snippets aligned with upstream docx_context (7 lines starting 4 lines
/// before the failing line).
fn map_jinja_error(err: &minijinja::Error, prepared: &str, part: &str) -> RenderError {
    let kind = match err.kind() {
        ErrorKind::SyntaxError => TemplateErrorKind::Syntax,
        ErrorKind::UndefinedError => TemplateErrorKind::Undefined,
        _ if is_python_value_error(err) => TemplateErrorKind::InvalidArgument,
        _ => TemplateErrorKind::Other,
    };
    let context = if let Some(lineno) = err.line() {
        let start = lineno.saturating_sub(4);
        prepared
            .lines()
            .skip(start)
            .take(7)
            .map(|line| strip_xml_tags().replace_all(line, "").into_owned())
            .collect()
    } else {
        Vec::new()
    };
    RenderError::Template {
        kind,
        part: part.to_string(),
        line: err.line(),
        message: err.to_string(),
        context,
    }
}

/// Static analysis for upstream
/// `DocxTemplate.get_undeclared_template_variables` (0.20.2,
/// template.py L894–927):
///
/// 1. Runs [`patch_xml`] over the output equivalent to `xml_to_string` of the
///    main document's `w:body` subtree (upstream
///    `self.patch_xml(self.xml_to_string(temp_doc._element.body))`);
/// 2. Appends in order the patch output of the **root element** (w:hdr /
///    w:ftr) of every non-empty-blob header/footer in the main rels (upstream
///    walks rels twice: headers first, then footers; concatenation order does
///    not affect the resulting set);
/// 3. Parses with a bare jinja environment and collects undeclared variables —
///    loop variables/macro arguments and such are excluded automatically by
///    jinja's meta analysis.
///
/// Returns a sorted set for stable display. This low-level template-crate
/// function takes no context or custom environment; the high-level
/// `DocxTemplate` facade provides a context/key diff entry point.
pub fn find_undeclared_variables(
    doc_xml: &str,
    story_xmls: &[String],
) -> Result<BTreeSet<String>, RenderError> {
    let mut combined = patched_body_xml(doc_xml)?;
    for story in story_xmls {
        let story = patched_root_xml(story)?;
        if story.len() > MAX_RENDERED_XML_BYTES.saturating_sub(combined.len()) {
            return Err(rendered_xml_limit(MAIN_PART));
        }
        combined.push_str(&story);
    }
    let env = build_jinja_env(false);
    let template = env
        .template_from_str(&combined)
        .map_err(|e| map_jinja_error(&e, &combined, MAIN_PART))?;
    Ok(template.undeclared_variables(false).into_iter().collect())
}

/// Parses the main document, serializes the w:body subtree, and patches it
/// (introspection only; whitespace is not stripped).
fn patched_body_xml(src_xml: &str) -> Result<String, RenderError> {
    let doc = parse_for_introspect(src_xml, MAIN_PART)?;
    // The parse-tree root is the top-level w:document (there is no virtual
    // Document layer).
    let document = doc.root();
    if !is_word_tag(&doc, document, "document") {
        return Err(introspect_structure_error());
    }
    let body = doc
        .children(document)
        .iter()
        .copied()
        .find(|&id| is_word_tag(&doc, id, "body"))
        .ok_or_else(introspect_structure_error)?;
    let serialized = doc
        .try_serialize_subtree(body, MAX_RENDERED_XML_BYTES)
        .map_err(|_| rendered_xml_limit(MAIN_PART))?;
    let patched = patch_xml(&serialized);
    if patched.len() > MAX_RENDERED_XML_BYTES {
        return Err(rendered_xml_limit(MAIN_PART));
    }
    Ok(patched)
}

/// Parses a story part such as header/footer, serializes its root element
/// (w:hdr / w:ftr), and patches it.
fn patched_root_xml(src_xml: &str) -> Result<String, RenderError> {
    let doc = parse_for_introspect(src_xml, MAIN_PART)?;
    let serialized = doc
        .try_serialize_subtree(doc.root(), MAX_RENDERED_XML_BYTES)
        .map_err(|_| rendered_xml_limit(MAIN_PART))?;
    let patched = patch_xml(&serialized);
    if patched.len() > MAX_RENDERED_XML_BYTES {
        return Err(rendered_xml_limit(MAIN_PART));
    }
    Ok(patched)
}

/// Strict parsing for the introspection path (a well-formed docx must pass;
/// failures are reported as XML errors).
fn parse_for_introspect(src_xml: &str, part: &str) -> Result<XmlDocument, RenderError> {
    XmlDocument::parse_strict(src_xml, &XmlLimits::default()).map_err(|source| RenderError::Xml {
        part: part.to_string(),
        source,
    })
}

/// Whether the node is a w:-namespace element with the given local name.
fn is_word_tag(doc: &XmlDocument, id: docxtpl_xml::NodeId, local: &str) -> bool {
    doc.tag(id)
        .is_some_and(|q| q.ns == ns_uri::W && q.local == local)
}

/// Malformed introspection input (missing document/body, etc.; unreachable
/// for a normal docx).
fn introspect_structure_error() -> RenderError {
    RenderError::Template {
        kind: TemplateErrorKind::Other,
        part: MAIN_PART.to_string(),
        line: None,
        message: "introspection failed: the document lacks the w:document/w:body structure"
            .to_string(),
        context: Vec::new(),
    }
}

#[cfg(test)]
mod limit_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn output_writer_stops_before_exceeding_limit() {
        let mut output = LimitedOutput {
            bytes: Vec::new(),
            max: 4,
            exceeded: false,
        };
        output.write_all(b"abcd").unwrap();
        assert!(output.write_all(b"e").is_err());
        assert!(output.exceeded);
        assert_eq!(output.bytes, b"abcd");
    }

    #[test]
    fn looping_template_exhausts_fuel() {
        let mut env = build_jinja_env(false);
        env.set_fuel(Some(100));
        let error = render_inline_value(
            &env,
            "{% for i in range(1000) %}x{% endfor %}",
            Value::from_serialize(json!({})),
            MAIN_PART,
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                RenderError::Limit {
                    kind: "template_fuel",
                    ..
                }
            ),
            "{error:?}"
        );
    }
}

#[cfg(test)]
mod jinja_compat_tests {
    use super::*;

    fn render(template: &str, context: &RenderContext) -> String {
        let env = build_jinja_env(false);
        let (root, pending_images) =
            context_to_minijinja(context, MAIN_PART).expect("valid context");
        assert!(pending_images.is_empty());
        render_inline_value(&env, template, root, MAIN_PART).expect("template should render")
    }

    #[test]
    fn json_and_explicit_objects_render_in_insertion_order() {
        let json =
            serde_json::from_str(r#"{"mapping":{"z":3,"a":1,"m":2}}"#).expect("valid JSON object");
        let json_context = RenderContext::from_json(&json);
        let template = r#"{% for key, value in mapping|items %}{{ key }}={{ value }};{% endfor %}"#;
        assert_eq!(render(template, &json_context), "z=3;a=1;m=2;");

        let mut explicit_context = RenderContext::new();
        explicit_context.insert(
            "mapping",
            RenderValue::object(vec![
                ("z".to_string(), RenderValue::from(3_i64)),
                ("a".to_string(), RenderValue::from(1_i64)),
                ("m".to_string(), RenderValue::from(2_i64)),
            ]),
        );
        assert_eq!(render(template, &explicit_context), "z=3;a=1;m=2;");
    }

    #[test]
    fn whitelisted_python_methods_match_jinja() {
        let json = serde_json::from_str(
            r#"{
                "mapping":{"x":1,"y":2},
                "text":"  ab cd  ",
                "duplicate":"a a b",
                "values":[1,2,2]
            }"#,
        )
        .expect("valid JSON object");
        let context = RenderContext::from_json(&json);
        let cases = [
            (
                "mapping methods",
                "{% for k,v in mapping.items() %}{{k}}={{v}};{% endfor %}|{{mapping.keys()|join(',')}}|{{mapping.values()|join(',')}}|{{mapping.get('x')}}|{{mapping.get('z','D')}}",
                "x=1;y=2;|x,y|1,2|1|D",
            ),
            (
                "string methods",
                "{{text.split()}}|{{duplicate.split(None,1)}}|{{text.startswith('  ab')}}|{{text.endswith('  ')}}|{{'abcabc'.count('ab')}}|{{'éxé'.index('é',1)}}|{{'abc'.startswith(('x','ab'))}}|{{'abc'.count('',3,1)}}",
                "['ab', 'cd']|['a', 'a b']|True|True|2|2|True|0",
            ),
            (
                "sequence methods",
                "{{values.count(2)}}|{{values.index(2)}}|{{values.index(2,2)}}",
                "2|1|2",
            ),
        ];
        for (name, template, expected) in cases {
            assert_eq!(render(template, &context), expected, "{name}");
        }

        assert_eq!(
            render(
                "{{a.split(None,0)}}|{{b.split(None,1)}}|{{c.split(None,1)}}",
                &RenderContext::from_json(
                    &serde_json::from_str(r#"{"a":"   ","b":"a a b","c":"  a a b  "}"#).unwrap(),
                ),
            ),
            "[]|['a', 'a b']|['a', 'a b  ']"
        );

        let env = build_jinja_env(false);
        let error =
            render_inline_value(&env, "{{'abc'.index('',3,1)}}", Value::UNDEFINED, MAIN_PART)
                .expect_err("an empty needle does not match an invalid slice range");
        assert!(error.to_string().contains("substring not found"), "{error}");
        assert_eq!(error.kind(), Some(TemplateErrorKind::InvalidArgument));

        for template in [
            "{{'abc'.split('')}}",
            "{{[1,2].index(3)}}",
            "{{'abc'|wordwrap(0)}}",
            "{{'not-a-number'|filesizeformat}}",
            "{{ {'bad\u{b}key':'v'}|xmlattr }}",
            "{{ [[1]]|urlencode }}",
            "{{ '%q'|format(1) }}",
            "{{ '%'|format(1) }}",
        ] {
            let error = render_inline_value(&env, template, Value::UNDEFINED, MAIN_PART)
                .expect_err("Python ValueError surface must fail");
            assert_eq!(
                error.kind(),
                Some(TemplateErrorKind::InvalidArgument),
                "{template}: {error}"
            );
        }

        for template in [
            "{{ '%s'|format('x', 'y') }}",
            "{{ 'literal'|format('x') }}",
            "{{ '%s'|format('x', y='z') }}",
        ] {
            let error = render_inline_value(&env, template, Value::UNDEFINED, MAIN_PART)
                .expect_err("extra or mixed printf arguments must fail");
            assert_eq!(error.kind(), Some(TemplateErrorKind::Other), "{template}");
        }

        let error = collect_split_parts_with_limit(["a", "b", "c"], 2)
            .expect_err("split item collection must stop at its intermediate budget");
        assert!(error.to_string().contains("intermediate items"), "{error}");
    }

    #[test]
    fn added_deterministic_filters_and_globals_match_jinja() {
        let context = RenderContext::new();
        let filters = concat!(
            "[{{'abc'|center(6)}}]|{{1000|filesizeformat}}|",
            "{{ -1500|filesizeformat }}|{{0.5|filesizeformat}}|",
            "{{true|filesizeformat}}|{{1024|filesizeformat(true)}}|",
            "{{'<p>Hello&nbsp; <b>world &CounterClockwiseContourIntegral;</b></p>'|striptags}}|",
            "{{'foo bar baz qux'|truncate(9)}}|",
            "{{'foo bar baz qux'|truncate(9,true)}}|",
            "{{'Hello, world! 42'|wordcount}}/{{123|wordcount}}/{{none|wordcount}}|",
            "{{'one two three'|wordwrap(7)}}",
        );
        assert_eq!(
            render(filters, &context),
            concat!(
                "[ abc  ]|1.0 kB|-1500 Bytes|0 Bytes|1 Byte|1.0 KiB|",
                "Hello\u{a0} world ∳|foo...|foo ba...|3/1/1|one two\nthree"
            )
        );
        for (template, expected) in [
            ("{{'你好 世界'|wordwrap(3)}}", "你好\n世界"),
            (
                "{{'alpha-beta gamma'|wordwrap(6,true,'/',false)}}",
                "alpha-/beta/gamma",
            ),
            ("{{'你好世界 测试'|wordwrap(4)}}", "你好世界\n测试"),
            (
                "{{'long-hyphenated-word'|wordwrap(10,true,None,false)}}",
                "long-hyphe\nnated-word",
            ),
            ("{{'abcdefghijk'|wordwrap(5,false)}}", "abcdefghijk"),
            ("{{'a\\tb c'|wordwrap(3)}}", "a\tb\nc"),
            ("{{'a\\n\\nb'|wordwrap(3)}}", "a\n\nb"),
        ] {
            assert_eq!(render(template, &context), expected, "{template}");
        }

        let globals = concat!(
            "{% macro item() %}x{% endmacro %}",
            "{% set c=cycler('odd','even') %}",
            "{{c.current}}/{{c.next()}}/{{c.next()}}/{{c.current}}/",
            "{{c.reset() or '-'}}/{{c.current}}|",
            "{% set j=joiner('|') %}{{j()}}a{{j()}}b{{j()}}c|",
            "{% set k=joiner(sep='~') %}{{k()}}a{{k()}}b|",
            "{{range is callable}}/{{c is callable}}/{{j is callable}}/",
            "{{item is callable}}/{{namespace() is callable}}",
        );
        assert_eq!(
            render(globals, &context),
            "odd/odd/even/odd/-/odd|a|b|c|a~b|True/False/True/True/False"
        );

        // The random-valued defaults cannot be compared byte-for-byte with a
        // separate Python process, but singleton/range invariants prove that
        // the standard Jinja names are registered with compatible signatures.
        assert_eq!(render("{{['only']|random}}", &context), "only");
        assert_eq!(render("{{randrange(7, 8)}}", &context), "7");
        assert!(!render("{{lipsum(1)}}", &context).is_empty());

        let env = build_jinja_env(false);
        let error = render_inline_value(&env, "{{cycler(item='x')}}", Value::UNDEFINED, MAIN_PART)
            .expect_err("cycler does not accept keyword arguments");
        assert!(error.to_string().contains("keyword"), "{error}");
    }

    #[test]
    fn arbitrary_precision_json_integers_are_exact_or_rejected() {
        let json = serde_json::from_str(
            r#"{"positive":18446744073709551617,"negative":-9223372036854775809}"#,
        )
        .expect("arbitrary precision JSON");
        let context = RenderContext::from_json(&json);
        assert_eq!(
            render(
                "{{positive}}|{{negative}}|{{positive|tojson}}|{{negative|tojson}}",
                &context,
            ),
            "18446744073709551617|-9223372036854775809|18446744073709551617|-9223372036854775809"
        );

        for spelling in [
            // 2^127: the first positive value outside signed i128.
            "170141183460469231731687303715884105728",
            // u128::MAX must not enter MiniJinja's lossy U128 arithmetic.
            "340282366920938463463374607431768211455",
            // One beyond u128 additionally guards serde_json's arbitrary range.
            "340282366920938463463374607431768211456",
        ] {
            let too_large = serde_json::from_str(&format!(r#"{{"n":{spelling}}}"#))
                .expect("valid arbitrary precision JSON");
            let error = context_to_minijinja(&RenderContext::from_json(&too_large), MAIN_PART)
                .expect_err("integer above i128 must be rejected before MiniJinja arithmetic");
            assert_eq!(error.kind(), Some(TemplateErrorKind::InvalidArgument));
            assert!(error.to_string().contains("signed 128-bit"), "{error}");
        }
    }

    #[test]
    fn expanding_filters_reject_unbounded_size_arguments() {
        let env = build_jinja_env(false);
        let over = MAX_RENDERED_XML_BYTES + 1;
        let half_over = MAX_RENDERED_XML_BYTES / 2 + 1;
        let templates = [
            format!("{{{{'x'|center({over})}}}}"),
            format!("{{{{'x'|wordwrap({over})}}}}"),
            format!("{{{{'x'|truncate({over})}}}}"),
            format!("{{{{'%{half_over}s%{half_over}s'|format('x','y')}}}}"),
            format!("{{{{'%*s'|format({over},'x')}}}}"),
        ];
        for template in templates {
            let error = render_inline_value(&env, &template, Value::UNDEFINED, MAIN_PART)
                .expect_err("expanding filter argument must be bounded");
            assert!(error.to_string().contains("rendered XML limit"), "{error}");
        }
    }

    #[test]
    fn public_json_render_rejects_non_object_context() {
        let error = render_document_xml(
            "unused",
            &serde_json::json!([1, 2]),
            &RenderOptions::compat(),
        )
        .expect_err("top-level arrays must not silently become an empty context");
        assert_eq!(error.kind(), Some(TemplateErrorKind::InvalidArgument));
        assert_eq!(error.part(), MAIN_PART);
        assert!(error.to_string().contains("must be an object"), "{error}");
    }

    #[test]
    fn live_python_oracle_for_new_compat_surface() {
        let Ok(python) = std::env::var("DOCXTPL_PYTHON_ORACLE") else {
            return;
        };
        let script = r#"from jinja2 import Environment
cases = [
    ("{{d.get('z','D')}}|{{s.split(None,1)}}|{{xs.count(2)}}", {'d':{'x':1},'s':'a a b','xs':[1,2,2]}),
    ("[{{'abc'|center(6)}}]|{{1000|filesizeformat}}|{{ -1500|filesizeformat }}|{{0.5|filesizeformat}}|{{true|filesizeformat}}|{{'foo bar baz qux'|truncate(9)}}", {}),
]
for template, context in cases:
    print(Environment().from_string(template).render(**context))
"#;
        let output = std::process::Command::new(python)
            .args(["-c", script])
            .output()
            .expect("run configured Python oracle");
        assert!(output.status.success());
        let oracle = String::from_utf8(output.stdout)
            .expect("UTF-8 oracle output")
            .replace("\r\n", "\n");

        let context = RenderContext::from_json(
            &serde_json::from_str(r#"{"d":{"x":1},"s":"a a b","xs":[1,2,2]}"#).unwrap(),
        );
        let rust = format!(
            "{}\n{}\n",
            render(
                "{{d.get('z','D')}}|{{s.split(None,1)}}|{{xs.count(2)}}",
                &context,
            ),
            render(
                "[{{'abc'|center(6)}}]|{{1000|filesizeformat}}|{{ -1500|filesizeformat }}|{{0.5|filesizeformat}}|{{true|filesizeformat}}|{{'foo bar baz qux'|truncate(9)}}",
                &RenderContext::new(),
            )
        );
        assert_eq!(rust, oracle);
    }

    #[test]
    fn common_jinja_filters_match_python_jinja() {
        let json = serde_json::from_str(
            r#"{
                "word":"Élan",
                "padded":"  hi  ",
                "values":[3,1,2],
                "users":[
                    {"name":"Ada","active":true},
                    {"name":"Bob","active":false}
                ]
            }"#,
        )
        .expect("valid JSON object");
        let context = RenderContext::from_json(&json);
        let template = concat!(
            r#"{{ word|lower }}|{{ word|upper }}|{{ padded|trim }}|"#,
            r#"{{ values|sort|join(",") }}|{{ values|sum }}|"#,
            r#"{{ users|map(attribute="name")|join(",") }}|"#,
            r#"{{ users|selectattr("active")|map(attribute="name")|join(",") }}"#,
        );

        assert_eq!(
            render(template, &context),
            "élan|ÉLAN|hi|1,2,3|6|Ada,Bob|Ada"
        );
    }

    #[test]
    fn common_jinja_tests_match_python_jinja() {
        let json = serde_json::from_str(r#"{"none":null,"mapping":{"x":1},"values":[1,2,3]}"#)
            .expect("valid JSON object");
        let context = RenderContext::from_json(&json);
        let template = concat!(
            r#"{% if missing is undefined %}u{% endif %}|"#,
            r#"{% if none is none %}n{% endif %}|"#,
            r#"{% if 9 is odd %}o{% endif %}|"#,
            r#"{% if 12 is divisibleby 3 %}d{% endif %}|"#,
            r#"{% if mapping is mapping %}m{% endif %}|"#,
            r#"{% if values is sequence %}s{% endif %}|"#,
            r#"{% if "ABC" is upper %}U{% endif %}|"#,
            r#"{% if 2 in values %}i{% endif %}"#,
        );

        assert_eq!(render(template, &context), "u|n|o|d|m|s|U|i");
    }

    #[test]
    fn unicode_json_and_urlencode_features_match_python_jinja() {
        let json = serde_json::from_str(
            r#"{"café":"ok","用户":"张三","json_value":"<tag>&'","url_value":"a b/c?d=e&x=1"}"#,
        )
        .expect("valid JSON object");
        let context = RenderContext::from_json(&json);
        let template = concat!(
            r#"{{ café }}|{{ 用户 }}|"#,
            r#"{{ "straße"|upper }}|{{ "ÉLAN"|lower }}|{{ "élan"|capitalize }}|"#,
            r#"{{ json_value|tojson }}|{{ url_value|urlencode }}"#,
        );

        assert_eq!(
            render(template, &context),
            r#"ok|张三|STRASSE|élan|Élan|"\u003ctag\u003e\u0026\u0027"|a%20b/c%3Fd%3De%26x%3D1"#
        );
    }

    #[test]
    fn tojson_and_mapping_urlencode_match_python_jinja_defaults() {
        let json = serde_json::from_str(
            r#"{
                "json_object":{"z":3,"a":1,"m":{"é":"张"}},
                "query":{"q":"my search","slash":"a/b","none":null}
            }"#,
        )
        .expect("valid JSON object");
        let context = RenderContext::from_json(&json);
        assert_eq!(
            render("{{ json_object|tojson }}|{{ query|urlencode }}", &context),
            r#"{"a": 1, "m": {"\u00e9": "\u5f20"}, "z": 3}|q=my+search&slash=a%2Fb&none=None"#
        );
        assert_eq!(
            render("{{ json_object|tojson(indent=2) }}", &context),
            "{\n  \"a\": 1,\n  \"m\": {\n    \"\\u00e9\": \"\\u5f20\"\n  },\n  \"z\": 3\n}"
        );
    }

    #[test]
    fn autoescape_plain_scalar_matches_python_jinja() {
        let mut context = RenderContext::new();
        context.insert("value", "<&>\"'");
        let env = build_jinja_env(true);
        let (root, pending_images) =
            context_to_minijinja(&context, MAIN_PART).expect("valid context");
        assert!(pending_images.is_empty());
        assert_eq!(
            render_inline_value(&env, "{{ value }}", root, MAIN_PART)
                .expect("autoescaped scalar should render"),
            "&lt;&amp;&gt;&#34;&#39;"
        );
    }

    #[test]
    fn python_container_repr_and_float_spelling_match_jinja() {
        let json = serde_json::from_str(
            r#"{
                "values":["<",true,null,0.0000001,100000000000000000000.0],
                "query":{"items":["x",1],"small":0.0000001,"large":100000000000000000000.0}
            }"#,
        )
        .expect("valid JSON object");
        let context = RenderContext::from_json(&json);
        let env = build_jinja_env(true);
        let (root, _) = context_to_minijinja(&context, MAIN_PART).expect("valid context");
        assert_eq!(
            render_inline_value(&env, "{{ values }}|{{ query|urlencode }}", root, MAIN_PART)
                .expect("container values should render"),
            concat!(
                "[&#39;&lt;&#39;, True, None, 1e-07, 1e+20]|",
                "items=%5B%27x%27%2C+1%5D&amp;small=1e-07&amp;large=1e%2B20"
            )
        );
    }

    #[test]
    fn default_formatter_uses_python_container_and_float_spelling() {
        let json =
            serde_json::from_str(r#"{"values":["x",true,null,0.0000001],"mapping":{"x":"y"}}"#)
                .expect("valid JSON object");
        let context = RenderContext::from_json(&json);
        assert_eq!(
            render("{{ values }}|{{ mapping }}", &context),
            "['x', True, None, 1e-07]|{'x': 'y'}"
        );
    }

    #[test]
    fn stringifying_filters_use_python_container_and_float_spelling() {
        let json = serde_json::from_str(
            r#"{
                "values":["x",true,null,0.0000001],
                "nested":[["x",true],{"a":null}],
                "small":0.0000001,
                "number":1.25
            }"#,
        )
        .expect("valid JSON object");
        let context = RenderContext::from_json(&json);

        assert_eq!(
            render(
                concat!(
                    "{{ values|string }}|",
                    "{{ '%s|%.2f|%s'|format(values, number, small) }}|",
                    "{{ nested|join('~') }}|",
                    "{{ values|replace('x','z') }}|",
                    "{{ small|replace('e-07','E') }}"
                ),
                &context,
            ),
            concat!(
                "['x', True, None, 1e-07]|",
                "['x', True, None, 1e-07]|1.25|1e-07|",
                "['x', True]~{'a': None}|",
                "['z', True, None, 1e-07]|1E"
            )
        );
    }

    #[test]
    fn join_attribute_and_replace_keyword_arguments_match_jinja() {
        let json = serde_json::from_str(
            r#"{
                "users":[
                    {"profile":{"name":"Ada"}},
                    {"profile":{"name":"Bob"}}
                ],
                "rows":[["a",1],["b",2]],
                "values":["x","x"]
            }"#,
        )
        .expect("valid JSON object");
        let context = RenderContext::from_json(&json);

        assert_eq!(
            render(
                concat!(
                    "{{ users|join(',', attribute='profile.name') }}|",
                    "{{ rows|join(',', attribute=0) }}|",
                    "{{ values|replace(old='x',new='z',count=1) }}"
                ),
                &context,
            ),
            "Ada,Bob|a,b|['z', 'x']"
        );
    }

    #[test]
    fn stringifying_filters_preserve_jinja_autoescape_safety() {
        let json = serde_json::from_str(
            r#"{
                "unsafe":"a&b",
                "safe_value":"<b>x</b>",
                "delim":"<&>",
                "source":"<p>X</p>",
                "replacement":"<b>&</b>"
            }"#,
        )
        .expect("valid JSON object");
        let context = RenderContext::from_json(&json);
        let env = build_jinja_env(true);
        let (root, _) = context_to_minijinja(&context, MAIN_PART).expect("valid context");
        assert_eq!(
            render_inline_value(
                &env,
                concat!(
                    "{{ [unsafe, safe_value|safe]|join(delim) }}|",
                    "{{ source|replace('X', replacement|safe) }}|",
                    "{{ source|replace('X', replacement) }}|",
                    "{{ '<b>'|safe|replace('<', 'X') }}|",
                    "{{ source|safe|replace('X', replacement|safe) }}"
                ),
                root,
                MAIN_PART,
            )
            .expect("safe values should retain Jinja escaping semantics"),
            concat!(
                "a&amp;b&lt;&amp;&gt;<b>x</b>|",
                "&lt;p&gt;<b>&</b>&lt;/p&gt;|",
                "&lt;p&gt;&lt;b&gt;&amp;&lt;/b&gt;&lt;/p&gt;|",
                "Xb>|<p><b>&</b></p>"
            )
        );
    }

    #[test]
    fn replace_streams_expansion_through_the_output_limit() {
        assert_eq!(
            replace_text_limited("abc", "", "-", None, 16).expect("bounded replacement"),
            "-a-b-c-"
        );
        let error = replace_text_limited("aaaa", "a", "bbbb", None, 8)
            .expect_err("multiplicative expansion must stop at the limit");
        assert!(error.to_string().contains("rendered XML limit"), "{error}");
    }

    #[test]
    fn python_repr_escapes_nonprinting_unicode_like_python() {
        assert_eq!(
            python_string_repr("\u{a0}\u{85}\u{2028}张\u{200b}\u{e0001}").unwrap(),
            r#"'\xa0\x85\u2028张\u200b\U000e0001'"#
        );
    }

    #[test]
    fn tojson_matches_python_float_indent_and_type_errors() {
        let context = RenderContext::new();
        for (value, expected) in [
            (1.234_567e-5, "1.234567e-05"),
            (1e-4, "0.0001"),
            (9.999e-5, "9.999e-05"),
            (1e15, "1000000000000000.0"),
            (1e16, "1e+16"),
            (-1e-5, "-1e-05"),
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.2e100, "1.2e+100"),
            (1.234_567_890_123_456_7e19, "1.2345678901234567e+19"),
        ] {
            assert_eq!(python_float_display(value, true), expected);
            assert_eq!(python_float_display(value, false), expected);
        }
        assert_eq!(
            render(
                "{{ [1e-5, 1e-6, 1e16, 1e400, -1e400, 1e400-1e400]|tojson }}",
                &context
            ),
            "[1e-05, 1e-06, 1e+16, Infinity, -Infinity, NaN]"
        );
        assert_eq!(
            render("{{ {'a':[1,2]}|tojson(indent='--') }}", &context),
            "{\n--\"a\": [\n----1,\n----2\n--]\n}"
        );
        assert_eq!(
            render("{{ {2:'two',10:'ten'}|tojson }}", &context),
            r#"{"2": "two", "10": "ten"}"#
        );
        assert_eq!(
            render("{{ {none:'nil'}|tojson }}", &context),
            r#"{"null": "nil"}"#
        );
        assert_eq!(
            render("{{ {true:'bool',1:'int'}|tojson }}", &context),
            r#"{"true": "int"}"#
        );
        assert_eq!(
            render("{{ {true:'one',0:'zero'}|tojson }}", &context),
            r#"{"0": "zero", "true": "one"}"#
        );

        let mut control_context = RenderContext::new();
        control_context.insert("value", "a\u{7f}b");
        assert_eq!(
            render("{{ value|tojson }}", &control_context),
            r#""a\u007fb""#
        );

        for &value in &[
            51.248_178_375_505_404_f64,
            -93.311_370_376_880_33,
            2.003_039_774_426_776_2e-253,
            7.101_215_824_554_616e260,
        ] {
            let serialized = serde_json::to_string(&value).expect("serialize finite float");
            let parsed: JsonValue = serde_json::from_str(&serialized).expect("parse finite float");
            assert_eq!(
                parsed.as_f64().expect("JSON number").to_bits(),
                value.to_bits(),
                "{serialized}"
            );
        }

        let env = build_jinja_env(false);
        for source in [
            "{{ missing|tojson }}",
            "{{ [missing]|tojson }}",
            "{{ range(2)|tojson }}",
            "{{ {'a':1}|tojson(indent=2.0) }}",
            "{{ {'a':1}|tojson(indent=missing) }}",
        ] {
            let err = render_inline_value(&env, source, Value::UNDEFINED, MAIN_PART)
                .expect_err("Python rejects this tojson input");
            assert!(
                err.to_string().contains("not JSON serializable")
                    || err.to_string().contains("indent"),
                "unexpected error for {source}: {err}"
            );
        }
    }

    #[test]
    fn urlencode_accepts_general_iterables_like_jinja() {
        let context = RenderContext::new();
        assert_eq!(render("{{ range(0)|urlencode }}", &context), "");
        let env = build_jinja_env(false);
        let err = render_inline_value(
            &env,
            "{{ range(2)|urlencode }}",
            Value::UNDEFINED,
            MAIN_PART,
        )
        .expect_err("range elements are not key/value pairs");
        assert!(err.to_string().contains("not iterable") || err.to_string().contains("two values"));
    }

    #[test]
    fn forceescape_and_xmlattr_match_jinja() {
        let json = serde_json::from_str(
            r#"{"attrs":{"id":"a&b","title":"\"q\"","none":null,"seq":["x"]}}"#,
        )
        .expect("valid JSON object");
        let context = RenderContext::from_json(&json);
        assert_eq!(
            render(
                "{{ '<b>&amp;</b>'|safe|forceescape }}|{{ attrs|xmlattr }}|{{ attrs|xmlattr(false) }}",
                &context
            ),
            concat!(
                "&lt;b&gt;&amp;amp;&lt;/b&gt;|",
                " id=\"a&amp;b\" title=\"&#34;q&#34;\" seq=\"[&#39;x&#39;]\"|",
                "id=\"a&amp;b\" title=\"&#34;q&#34;\" seq=\"[&#39;x&#39;]\""
            )
        );

        let env = build_jinja_env(false);
        let err = render_inline_value(
            &env,
            "{{ {'bad key':'v'}|xmlattr }}",
            Value::UNDEFINED,
            MAIN_PART,
        )
        .expect_err("invalid attribute names must be rejected");
        assert!(err
            .to_string()
            .contains("Invalid character in attribute name"));

        let err = render_inline_value(
            &env,
            "{{ {'bad\u{b}key':'v'}|xmlattr }}",
            Value::UNDEFINED,
            MAIN_PART,
        )
        .expect_err("Jinja rejects ASCII vertical tab in attribute names");
        assert!(err
            .to_string()
            .contains("Invalid character in attribute name"));

        let json = serde_json::from_str(r#"{"attrs":{"id":"a&b"}}"#).expect("valid JSON object");
        let context = RenderContext::from_json(&json);
        assert_eq!(
            render(
                "{{ attrs|xmlattr(autospace=false) }}|{{ attrs|xmlattr(none) }}|{{ attrs|xmlattr('yes') }}|{{ attrs|xmlattr|escape }}",
                &context,
            ),
            concat!(
                "id=\"a&amp;b\"|id=\"a&amp;b\"| id=\"a&amp;b\"|",
                " id=&#34;a&amp;amp;b&#34;"
            )
        );

        assert_eq!(
            render("{{ {'a\u{a0}b':'v'}|xmlattr }}", &RenderContext::new()),
            " a\u{a0}b=\"v\""
        );

        let env = build_jinja_env(true);
        let (root, _) = context_to_minijinja(&context, MAIN_PART).expect("valid context");
        assert_eq!(
            render_inline_value(&env, "{{ attrs|xmlattr|escape }}", root, MAIN_PART)
                .expect("autoescaped xmlattr should stay safe"),
            " id=\"a&amp;b\""
        );
    }
}

#[cfg(test)]
mod context_render_tests {
    use super::*;
    use crate::context::{ImageRegistry, ImageRels, ImageResolveError};
    use docxtpl_rich::{InlineImage, Listing, RichText, RichTextParagraph, RichTextProps};

    /// Fake registry that always returns rId9 (records the call count to
    /// verify idempotence).
    struct StubRegistry {
        fail: bool,
        calls: usize,
    }

    impl ImageRegistry for StubRegistry {
        fn resolve_image(&mut self, _image: &InlineImage) -> Result<ImageRels, ImageResolveError> {
            self.calls += 1;
            if self.fail {
                return Err(ImageResolveError {
                    message: "unrecognized image format".to_string(),
                });
            }
            Ok(ImageRels {
                blip_rid: "rId9".to_string(),
                hyperlink_rid: None,
            })
        }
    }

    #[test]
    fn shape_id_scans_unprefixed_id_only() {
        assert_eq!(shape_id_of("<w:document/>"), 1);
        // w:id / r:id are prefixed and not counted; docPr id=1 → 2
        let src = r#"<w:document><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:id="99"/></w:numPr></w:pPr>
<w:drawing><wp:inline><wp:docPr id="1" name="Picture 1"/></wp:inline></w:drawing></w:document>"#;
        assert_eq!(shape_id_of(src), 2);
        // Take the maximum +1
        let src = r#"<wp:docPr id="1"/><wp:docPr id="5"/><a:hlinkClick r:id="rId9"/>"#;
        assert_eq!(shape_id_of(src), 6);
    }

    #[test]
    fn rich_text_renders_as_markup_inside_run() {
        let mut props = RichTextProps::new();
        props.bold = true;
        let mut ctx = RenderContext::new();
        ctx.insert("rt", RichText::text_with("bold", &props));

        let src = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{{ rt }}</w:t></w:r></w:p></w:body></w:document>"#;
        let mut registry = StubRegistry {
            fail: false,
            calls: 0,
        };
        let outcome =
            render_document_xml_ctx(src, &ctx, &RenderOptions::compat(), &mut registry).unwrap();
        assert!(outcome.xml.contains("<w:b/>"), "{}", outcome.xml);
        assert!(outcome.xml.contains("bold"), "{}", outcome.xml);
        // No images: the registry must not be called
        assert_eq!(registry.calls, 0);
    }

    #[test]
    fn empty_rich_values_keep_python_object_truthiness_and_markup_identity() {
        let mut context = RenderContext::new();
        context.insert("rt", RichText::new());
        context.insert("rtp", RichTextParagraph::new());
        context.insert("listing", Listing::new(""));
        context.insert(
            "subdoc",
            RenderValue::Subdoc(
                crate::context::SubdocFragment::parse("").expect("empty fragment is valid"),
            ),
        );
        let env = build_jinja_env(true);
        let (root, pending) =
            context_to_minijinja(&context, MAIN_PART).expect("valid rich context");
        assert!(pending.is_empty());
        let template = concat!(
            "{% for v in [rt,rtp,listing,subdoc] %}",
            "{{v}}/{{v is string}}/{{v is escaped}}/",
            "{% if v %}T{% else %}F{% endif %}/{{v|escape}}/{{v|string}};",
            "{% endfor %}",
        );
        assert_eq!(
            render_inline_value(&env, template, root, MAIN_PART).expect("render rich identities"),
            "/False/True/T//;/False/True/T//;/False/True/T//;/False/True/T//;"
        );

        let mut nonempty = RenderContext::new();
        nonempty.insert("rt", RichText::text("x&y"));
        let (root, _) = context_to_minijinja(&nonempty, MAIN_PART).expect("valid rich context");
        assert_eq!(
            render_inline_value(&env, "{{rt}}|{{rt|escape}}|{{rt|string}}", root, MAIN_PART,)
                .expect("render nonempty rich value"),
            concat!(
                "<w:r><w:t xml:space=\"preserve\">x&amp;y</w:t></w:r>|",
                "<w:r><w:t xml:space=\"preserve\">x&amp;y</w:t></w:r>|",
                "&lt;w:r&gt;&lt;w:t xml:space=&#34;preserve&#34;&gt;",
                "x&amp;amp;y&lt;/w:t&gt;&lt;/w:r&gt;"
            )
        );

        let (root, _) = context_to_minijinja(&nonempty, MAIN_PART).expect("valid rich context");
        let transformed = render_inline_value(
            &env,
            "{{rt|center(60)}}|{{rt|replace('w','X')}}|{{rt|wordcount}}",
            root,
            MAIN_PART,
        )
        .expect("string-producing rich filters should render");
        assert!(
            transformed.contains("&lt;w:r&gt;") && transformed.contains("&lt;X:r&gt;"),
            "RichText center/replace results must be ordinary autoescaped strings: {transformed}"
        );
        assert!(!transformed.contains("<w:r>"), "{transformed}");

        let (root, _) = context_to_minijinja(&nonempty, MAIN_PART).expect("valid rich context");
        let error = render_inline_value(&env, "{{rt|truncate(9)}}", root, MAIN_PART)
            .expect_err("RichText-like objects have no Python string length");
        assert!(error.to_string().contains("expected string"), "{error}");
    }

    #[test]
    fn ordinary_strings_cannot_forge_image_placeholders() {
        let png_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/media/p4_dot2x1.png"
        );
        let image = InlineImage::from_path(png_path, None, None, None).unwrap();
        let forged = "\u{1}DOXTPLRSIMG0@\u{1}";
        let mut context = RenderContext::new();
        context.insert("forged", forged);
        // Images are registered during context conversion even when the
        // template does not reference them, which made the old fixed token
        // exploitable as index zero.
        context.insert("unused_image", image);

        let (root, pending) =
            context_to_minijinja(&context, MAIN_PART).expect("valid image context");
        assert!(!pending.is_empty());
        let rendered = render_inline_value(&build_jinja_env(false), "{{forged}}", root, MAIN_PART)
            .expect("render forged ordinary string");
        let mut registry = StubRegistry {
            fail: false,
            calls: 0,
        };
        let substituted = substitute_images(&rendered, &mut registry, &pending, 1, MAIN_PART)
            .expect("forged token must remain ordinary text");
        assert_eq!(substituted, forged);
        assert_eq!(registry.calls, 0);
    }

    #[test]
    fn image_substitution_stops_at_the_output_limit() {
        let png_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/media/p4_dot2x1.png"
        );
        let image = InlineImage::from_path(png_path, None, None, None).unwrap();
        let mut context = RenderContext::new();
        context.insert("image", image);
        let (_root, pending) =
            context_to_minijinja(&context, MAIN_PART).expect("valid image context");
        let rendered = pending.token(0);
        let mut registry = StubRegistry {
            fail: false,
            calls: 0,
        };
        let error =
            substitute_images_with_limit(&rendered, &mut registry, &pending, 1, MAIN_PART, 64)
                .expect_err("expanded image XML must honor the output budget");
        assert!(matches!(
            error,
            RenderError::Limit {
                kind: "rendered_xml_bytes",
                max: 64,
                ..
            }
        ));
        assert_eq!(registry.calls, 1);
    }

    #[test]
    fn inline_image_gets_registry_rids_and_shape_id() {
        let png_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/media/p4_dot2x1.png"
        );
        let image = InlineImage::from_path(png_path, None, None, None).unwrap();
        let mut ctx = RenderContext::new();
        ctx.insert("img", image);

        let src = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body><w:p><w:r><w:t>{{ img }}</w:t></w:r></w:p></w:body></w:document>"#;
        let mut registry = StubRegistry {
            fail: false,
            calls: 0,
        };
        let outcome =
            render_document_xml_ctx(src, &ctx, &RenderOptions::compat(), &mut registry).unwrap();
        assert!(outcome.xml.contains(r#"r:embed="rId9""#), "{}", outcome.xml);
        // shape_id=1 is renumbered from 1001 by the fix_docpr_ids post-processing
        assert!(
            outcome.xml.contains(r#"<wp:docPr id="1001""#),
            "{}",
            outcome.xml
        );
        assert_eq!(registry.calls, 1);
    }

    #[test]
    fn bad_image_maps_to_image_error_kind() {
        let bad_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/media/p4_bad.png"
        );
        let image = InlineImage::from_path(bad_path, None, None, None).unwrap();
        let mut ctx = RenderContext::new();
        ctx.insert("img", image);

        let src = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{{ img }}</w:t></w:r></w:p></w:body></w:document>"#;
        // The registry succeeds (virtual rId), but the genuinely bad bytes
        // fail during the render_inline_image probe stage
        let mut registry = StubRegistry {
            fail: false,
            calls: 0,
        };
        let error = render_document_xml_ctx(src, &ctx, &RenderOptions::compat(), &mut registry)
            .unwrap_err();
        assert_eq!(error.kind(), Some(TemplateErrorKind::Image));
        assert_eq!(
            error.kind().unwrap().oracle_exception(),
            "UnrecognizedImageError"
        );
    }

    /// P5: after rendering a header story part, docPr is not renumbered; new
    /// images take this part's next_id (existing id=5 → new image id=6), and
    /// existing docPr is kept verbatim.
    #[test]
    fn story_part_keeps_local_docpr_ids_without_renumber() {
        let png_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/media/p4_dot2x1.png"
        );
        let image1 = InlineImage::from_path(png_path, None, None, None).unwrap();
        let image2 = InlineImage::from_path(png_path, None, None, None).unwrap();
        let mut ctx = RenderContext::new();
        ctx.insert("x", "HX");
        ctx.insert("img", image1);
        ctx.insert("img2", image2);

        let src = r#"<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:p><w:r><w:t>{{x}}{{img}}{{img2}}</w:t></w:r></w:p><w:p><w:r><w:drawing><wp:inline><wp:docPr id="5" name="Existing"/></wp:inline></w:drawing></w:r></w:p></w:hdr>"#;
        let mut registry = StubRegistry {
            fail: false,
            calls: 0,
        };
        let outcome = render_story_xml_ctx(
            src,
            &ctx,
            &RenderOptions::compat(),
            &mut registry,
            "word/header1.xml",
        )
        .unwrap();
        // The existing docPr id=5 is kept (in body mode fix_docpr_ids would
        // change it to 1003)
        assert!(
            outcome.xml.contains(r#"<wp:docPr id="5""#),
            "{}",
            outcome.xml
        );
        // The two new images share the same shape_id (upstream next_id is
        // re-evaluated without cache over the original tree, both
        // max(5)+1=6); body fix_docpr_ids does not apply to story parts.
        assert_eq!(
            outcome.xml.matches(r#"<wp:docPr id="6""#).count(),
            2,
            "{}",
            outcome.xml
        );
        assert!(outcome.xml.contains("HX"), "{}", outcome.xml);
        assert_eq!(registry.calls, 2);
    }

    /// P5: a header syntax error carries the concrete part name and has the
    /// Syntax category.
    #[test]
    fn story_syntax_error_carries_header_part_name() {
        let ctx = RenderContext::new();
        let src = r#"<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t>{% if x %}open</w:t></w:r></w:p></w:hdr>"#;
        let mut registry = StubRegistry {
            fail: false,
            calls: 0,
        };
        let error = render_story_xml_ctx(
            src,
            &ctx,
            &RenderOptions::compat(),
            &mut registry,
            "word/header1.xml",
        )
        .unwrap_err();
        assert_eq!(error.kind(), Some(TemplateErrorKind::Syntax));
        assert!(error.to_string().contains("word/header1.xml"), "{error}");
    }

    /// P5: footnotes output is the raw string — the template
    /// declaration/format is preserved; only jinja substitution and
    /// resolve_listing run, with no XML parse/re-serialization.
    #[test]
    fn footnotes_render_keeps_raw_declaration_and_bytes() {
        let mut ctx = RenderContext::new();
        ctx.insert("z", "ZZ");
        let src = "<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n".to_string()
            + r#"<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:footnote w:id="1"><w:p><w:r><w:t>FN {{z}}</w:t></w:r></w:p></w:footnote></w:footnotes>"#;
        let out =
            render_footnotes_xml_ctx(&src, &ctx, &RenderOptions::compat(), "word/footnotes.xml")
                .unwrap();
        // The declaration is kept verbatim (story/document mode would rewrite
        // the declaration uniformly)
        assert!(
            out.starts_with("<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n"),
            "{out}"
        );
        assert!(out.contains("FN ZZ"), "{out}");
    }

    /// P5: an InlineImage in footnotes is rejected by NullRegistry (DEV-0006).
    #[test]
    fn footnotes_reject_inline_image() {
        let png_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/media/p4_dot2x1.png"
        );
        let image = InlineImage::from_path(png_path, None, None, None).unwrap();
        let mut ctx = RenderContext::new();
        ctx.insert("img", image);
        let src = r#"<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:footnote w:id="1"><w:p><w:r><w:t>{{img}}</w:t></w:r></w:p></w:footnote></w:footnotes>"#;
        assert!(render_footnotes_xml_ctx(
            src,
            &ctx,
            &RenderOptions::compat(),
            "word/footnotes.xml"
        )
        .is_err());
    }
}
