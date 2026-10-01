# Changelog

## [Unreleased]

## [1.2.1] - 2026-10-01

- Added a bounded `RunTextIndex` MVP for literal search and safe single- or
  cross-run replacement in body/header/footer story paragraphs. It includes
  explicit formatting policies, structural boundaries, stale-index detection,
  and text/match/replacement-growth limits.
- Added cloneable `CancellationToken` support across plain and rich rendering,
  transactional post-processing, validation, and interruptible ZIP output.
- Added interruptible OPC save/write APIs. Cancelled atomic file saves preserve
  the previous destination; stream APIs explicitly permit partial output.
- Existing 1.2.0 APIs retain their behavior; all new capabilities are additive
  and opt-in.

## [1.2.0] - 2026-10-01

- Started the 1.2 editing path: `RenderedDocument::edit_package` lets callers
  modify the rendered OPC package before its final serialization, and every
  output method revalidates package integrity after editing.
- Added `FilePartSource::snapshot`, file-backed part replacement, and a
  snapshot-based add API. File-backed replacements remain streaming during ZIP
  output and fail if their source changes.
- Added an on-demand `PackageTransaction` undo journal and a rendered-document
  post-processing pipeline. Each pass commits independently; failed passes
  always roll back and can either abort the pipeline or emit a structured
  warning and continue.
- Added bounded body/header/footer story editing. Each selected story is parsed
  once, exposes one shared editable XML DOM for multiple operations, and is
  serialized at most once with the existing body/story namespace behavior.
- Extended JPEG probing to accept valid SOI/SOF images without JFIF or Exif
  application segments. The generic path safely skips variable-length marker
  segments, rejects truncated or invalid lengths, and uses the 72 dpi fallback;
  existing JFIF/Exif DPI selection remains unchanged.
- Added opt-in structured output metrics. `PackageWriteReport` distinguishes
  raw-copied and rewritten entries and reports file-backed/loaded/modified
  counts and final bytes; `RenderReport` adds package-open, render,
  post-processing, validation, and final ZIP-write timings.
- Added transactional relationship and media registration for post-processing.
  Exact owner/type/target/mode relationships and SHA-1-identical media are
  reused; new `rId` and `imageN` values fill deterministic gaps, path-backed
  media stays streaming, and Content Types changes roll back with the pass.
- Added exact package part-buffer residency snapshots and safe clean-part cache
  eviction. Only unmodified lazy parts backed by the still-open source ZIP are
  evicted; modified, reader-created, and file-backed content remains resident.
  ZIP write reports now include resident content bytes at serialization start.
- Completed the output safety closure: every package serialization validates
  OPC integrity, file saves use a synced same-directory temporary file before
  replacement, and failed validation or bounded writing preserves an existing
  destination. Part mutations enforce entry-count, per-entry, and total
  uncompressed package limits before changing state.
- Added deterministic bookmark and internal-hyperlink primitives to
  `StoryEditor`. Bookmark names are normalized and bounded, numeric ids fill
  gaps, repeated creation/linking is idempotent, and story write-back rejects
  duplicate or unmatched bookmarks and dangling internal anchors.
- Added existing-Drawing external hyperlink support. Post-processing can
  register an idempotent owner-scoped external relationship and attach it to
  both `wp:docPr` and `pic:cNvPr`; duplicate click nodes, wrong node types, and
  relationship ids outside the story's external hyperlinks are rejected.
- Made integration-test temporary directories honor `CARGO_TARGET_DIR`, so
  isolated and project-local build layouts no longer depend on a pre-existing
  workspace `target` directory.
- Replaced fixture ownership metadata with the neutral project identity
  `docxtpl-rs` and audited the candidate diff and DOCX archives for local paths,
  host addresses, SSH material, and user identifiers.
- Corrected the CLI process status so `--help` and `--version` exit successfully
  while actual argument errors continue to return status 2.

## [1.1.0] - 2026-09-29

- Added bounded parallel image probing and hashing with deterministic package
  mutation order and an explicit in-flight byte budget.
- Added lazy path-backed images and streaming media output to reduce peak memory
  for documents containing hundreds or thousands of images.
- Added configurable media compression, including an automatic policy that
  stores formats that are already compressed.
- Added fast paths for marker-free XML and an instance-scoped preprocessing
  cache that is invalidated when a path-backed template changes.
- Reduced repeated relationship and media allocation scans while preserving
  Python docxtpl-compatible output in compatible mode.
- Added large-image performance harnesses, package write regressions, Word COM
  acceptance automation, and Windows/Linux office-suite acceptance evidence.

## [1.0.0] - 2026-09-27

- Completed every 1.0 release gate for the immutable `v1.0.0-rc.3`
  candidate: Linux/Windows/macOS CI, Rust 1.85 MSRV, packaged-crate smoke,
  LibreOffice, Microsoft Word visual inspection, and all four bounded fuzz
  targets.
- Completed the final license disposition review. The workspace now declares
  `LGPL-2.1-only`, matching the pinned upstream baseline and conservatively
  treating this semantic port as a derivative work; the upstream Word-template
  provenance and license are explicit in the fixture manifest.
- Clarified the product boundary: docxtpl-rs is an independent, non-official
  Rust implementation. Python docxtpl remains only the pinned development-time
  compatibility oracle and is excluded from all published crate contents and
  runtime dependency graphs.

- Prepared `1.0.0-rc.3` after `v1.0.0-rc.2` showed that the release job used
  the wrong Python selector for the live Jinja tests; all oracle subprocesses
  now use the absolute path of the pinned virtual-environment interpreter.
- Prepared `1.0.0-rc.2` after the immutable `v1.0.0-rc.1` candidate exposed
  two fuzz-found XML recovery panics; invalid UTF-8 byte offsets and overflowing
  numeric character references now return normal parse errors with regression
  coverage.
- Parallelized the four bounded fuzz targets and fixed the release workflow to
  run Rust oracle tests with the pinned Python virtual environment.
- Added the default Jinja `random` filter and `lipsum`/`randrange` globals.
- `striptags` now decodes the complete HTML5 named-entity set while preserving
  Jinja's whitespace-before-unescape behavior.
- Aligned the `number` and `sequence` tests for booleans, strings, and mappings,
  and added a live expected-deviation gate for negative-divisor modulo.
- Added raw UTF-8 template rendering for `word/endnotes.xml`, parallel to the
  existing footnotes path.

All user-visible changes are recorded here (code spec §10). Semantic alignment
baseline: Python docxtpl 0.20.2.

- English-ification of docs and code: removed archived stage acceptance/report
  artifacts (P0–P3 acceptance, the P7 compatibility report, P7c/P7d performance
  and Word spot-check evidence, and the P8 API audit and release reports);
  decision records such as the ADRs and the living documentation are retained.
  All Rust comments and doc comments are now in English, and library error
  `Display` text and CLI output were anglicized in step; error types, matching
  patterns, and placeholders are unchanged — only the display language changed,
  so callers that string-match on error wording should take note.
- The English-ification was extended to the whole repository: all living
  documentation, every ADR (also renamed to English file slugs), Python
  generator/oracle scripts, Cargo package descriptions, and Rust test
  identifiers are now in English. Fixture context data values were anglicized
  and all templates, contexts, stage dumps, and oracle goldens were
  regenerated from Python docxtpl 0.20.2 (122 fixtures, same 118 ok / 4 expected
  negative-error baseline). Chinese remains only in load-bearing Unicode/CJK
  test vectors (CJK wordwrap, non-ASCII JSON keys, Python string repr, and the
  multilingual `rt_unicode`/live-oracle probes), where the characters
  themselves are the behavior under test. The two superseded Chinese root
  planning documents were removed.

### 1.0.0-rc.1 release candidate

- The workspace version is frozen at `1.0.0-rc.1`; internal path dependencies
  also carry exact registry versions; release metadata
  (repository/homepage/readme/keywords/categories) was completed.
- Added the full LGPL-2.1 text, the 1.0 release-line guide, and the release
  manual; the release candidate initially used `LGPL-2.1-or-later`, which the
  final 1.0 license review narrowed to `LGPL-2.1-only`. This candidate is the first public
  compatibility baseline; earlier development snapshots are not migration
  targets.
- Added a dependency-license gate and `.crate`-based clean-install smoke tests:
  isolated library-example build, CLI install, and separate DOCX rendering and
  validation; added a release-candidate CI that does not publish or tag
  automatically.
- The RustSec audit found and closed two denial-of-service advisories for
  `quick-xml 0.37.5` (RUSTSEC-2026-0194/0195): upgraded to 0.41. Disabled the
  unused `time` optional feature of `zip`, removing `time 0.3.44` — affected by
  RUSTSEC-2026-0009 and requiring a higher MSRV — from the dependency graph,
  while retaining every ZIP codec needed for OOXML reading and writing.

- P8 large-document path: default document/render/output budgets raised to
  600 MiB and the ZIP entry limit to 6000; added `ResourceLimits` and
  `DocxTemplate::open_with_limits`. File templates no longer keep the full
  compressed bytes resident; ordinary ZIP parts are decompressed and cached on
  demand, and unmodified entries are raw-copied on write-out; saving now streams
  the ZIP to a temporary file in the same directory before persisting, avoiding
  an extra full-output `Vec`; Subdocs inherit the main package limits.
  Compression-ratio, fuel, and CLI JSON protections are retained.
- Narrowed Jinja/value compatibility gaps: completed common Python methods on
  JSON dict/list/string, deterministic filters/globals/tests, RichText-like
  truthiness/Markup identity, `xmlattr` vertical-tab validation, and bounded
  `wordwrap` measured by Python character length; JSON integers preserve exact
  fidelity within signed i128, out-of-range values error explicitly, and the
  public JSON render entry point rejects non-object top-level values. The
  remaining VM/type-model and default-library-surface boundaries are registered
  as DEV-0018/DEV-0019.
- `RenderOptions::with_environment_configurator` supports Rust-native MiniJinja
  filters/tests/functions and applies the same configuration to the body,
  header/footer, core properties, and footnotes; propagation from the two
  high-level entry points to document/core already has integration regressions,
  while the remaining parts are guaranteed by the shared render orchestration,
  and the compatibility list honestly distinguishes implementation scope from
  direct evidence.
- InlineImage deferred placeholders now use an independently randomized token
  namespace per context conversion, avoiding accidental collisions between
  ordinary input and the old fixed token; this is not a cryptographic
  unpredictability guarantee. Residual object-semantics differences of
  transform-type filters are registered as DEV-0017.
- Fixed the differing empty-value semantics of `RichTextParagraph("")` and
  explicit `add("")`, and the truthiness of empty image anchors; added optional
  `title`/`descr` accessibility metadata to InlineImage (written to both
  `wp:docPr` and `pic:cNvPr`). filename/title/descr are checked for XML 1.0
  characters before image placement, with a 64 MiB budget on the aggregated
  post-escaping bytes.
- **API / potentially breaking**: `RenderOptions` no longer implements `Copy`
  because it holds a shared configurator (the `autoescape` getter now borrows);
  `InlineImage` gains public `title`/`descr` fields; `docxtpl_rs::Error` gains
  `InvalidXmlEncoding`; `docxtpl_rich::ImageError` gains `InvalidXmlMetadata`
  and `MetadataTooLarge { max }`. Downstream code relying on struct literals or
  exhaustive enums must account for these variants.
- Body, header/footer, core, and Subdoc XmlParts now accept, by the XML initial-
  byte pattern, a UTF-8 BOM and UTF-16/32 LE/BE, rejecting invalid scalars and
  unsupported/conflicting declarations; footnotes, as an upstream generic Part,
  remain UTF-8-only. Story relationships now match the official URI exactly; the
  same footnotes part in a multi-section document is rendered only once, to
  avoid previous output being reinterpreted as a template.
- `RenderSession` gains `new_subdoc_from_reader` / `new_subdoc_from_bytes`,
  sharing the merge implementation with the path entry point and covered by a
  final-DOCX full-byte equivalence regression.
- Post-Subdoc-freeze hardening completed missing-numbering-part creation/number
  restart, multi-section, namespace, custom properties, SmartArt, VML, and
  footnote merging; public boundaries remain: no docpath borrowing mode, no
  JSON Subdoc, deterministic `w:nsid`, and the intentional fix of the upstream
  dangling footnote relationship.
- The Python oracle/golden gate now derives its set dynamically from the
  manifest and stage files, validating report status, duplicates/omissions, and
  complete pairing; added table-driven live Jinja2 diffing.
- Template introspection gains an entry point for diffing against a typed
  context or a key set; image replacement gains an explicit lenient
  `allow_missing_pics` policy. Both keep their original strict defaults.
- The CLI gains a Python docxtpl 0.20.2-style direct-invocation syntax plus
  `-o/--overwrite` and `-q/--quiet`; direct invocation validates inputs and
  extensions and uses atomic `create_new` to prevent overwriting existing output
  by default. JSON context files for both invocation forms are limited to
  64 MiB. The existing `docxtpl render ... [--autoescape]` syntax stays
  compatible.
- Render-time allocation-time XML serialization, layered `resolve_listing`
  expansion, image replacement, and expansion-type filter output are bounded at
  64 MiB per single buffer; MiniJinja fuel is 10,000,000, and intermediate
  collections such as `split`/`urlencode`/`join` are additionally subject to a
  524,288-item budget. These are per-path budgets; there is no claim that the
  whole process or all simultaneously live allocations are strictly capped at
  64 MiB.
- Extreme `shape_id` and table `gridSpan` arithmetic uses saturating operations,
  avoiding divergent overflow behavior between debug and release builds on
  malicious values.
- Fixed a panic in the strict XML parser when prefix-slicing multibyte Unicode
  characters in a malformed `<!` declaration; added a targeted regression and a
  persisted property-test seed.
- Completed quality gates, oracle, MSRV, Office, and measured performance
  evidence on macOS 26.6.2 arm64.

## [0.8.0] - 2026-09-26

The P4–P7d stage history is retained below by topic (not in strict chronological
order); a stage entry's "not supported at the time" does not reflect the current
working-tree state — for items closed later, the top `[Unreleased]` section and
compatibility.md take precedence.

- **P7c/P7d: compatibility hardening, audit, and the 0.8.0 freeze (ADR-010)**.
  - Added 6 proptest properties covering OPC random/truncated bytes, arbitrary-
    input XML strict/recover, and marker-heavy `patch_xml` (256/512 cases each),
    with no panics on error paths.
  - Reviewed resource peaks for 127 templates; the default limits still cover
    the maximum of 23 entries, 833,014 B total decompressed, and a 32.156
    compression ratio; no high-risk package-validation defect was found.
  - Added `tests/p7c_audit.py` and a three-sample staged Windows release
    performance baseline, splitting open/render/write, excluding CLI startup,
    and recording peak RSS; a same-platform render regression over 20% can
    block; CI keeps Windows/Linux/macOS regressions, and clippy was extended to
    `--all-features`.
  - Froze the P7 compatibility report and the public DEV boundaries; the
    workspace version was bumped to 0.8.0; no crates.io publish or Git tag is
    included.
  - Follow-up audits completed `DocxTemplate::picture_map()` (upstream
    `get_pic_map`) plus reset/zipname-priority regressions, and explicitly
    retained three-platform, Office, continuous fuzz, and peak-memory evidence
    as pre-release evidence, no longer equating CI configuration with measured
    results.
  - Added the `p4_autoescape_rich` oracle and fixed DEV-0005, where rich values
    were escaped under autoescape; the baseline rose to 102 renders (98 MATCH +
    4 error categories), golden 102/100. Added duplicate/external story,
    SmartArt/VML/footnote rejection, and P7 descr/miss/registration-order tests;
    Subdoc raw strings became strictly validated opaque fragments.
  - Added four `cargo-fuzz` targets (OPC, strict/recover XML, patch_xml, and
    full from_bytes/render) and a weekly bounded fuzz workflow; added part-
    locating integration regressions for header images and core-property syntax
    errors, and cleaned up 224 DOCX files that differed only in ZIP timestamps.
  - Peak RSS collection was extended to Windows/Linux/macOS; performance
    comparisons explicitly reject cross-OS/architecture baselines, to avoid
    machine differences being misreported as performance regressions.
  - Completed the DEV-0008 path-mode/borrowing-mode API boundary and the
    DEV-0010 missing-numbering/number-restart rejection branches; when the core-
    properties part is missing it is rebuilt per python-docx, and non-UTF-8 core
    properties return an error carrying the part name.
  - On an Ubuntu 24.04 x86_64 VM (rustc 1.98.1), completed workspace, clippy,
    fmt, the oracle, and 15 Linux performance/RSS baselines; LibreOffice 24.2.7.2
    opened and resaved 8/8 representative DOCX. Fixed a new-Clippy report: JPEG
    markers do not require lazy initialization.
  - On Windows 11 Pro x64 with Microsoft Word 16.0.17932.20700 x64, completed
    open/save/reopen of 10 representative Rust outputs (10/10), manual
    inspection of 11 Word-exported pages (11/11), and verification of the
    ZIP/XML structure of resaved DOCX (10/10).

- **P4: RichText / RichTextParagraph / Listing / InlineImage (ADR-005)**.
  Added 17 `p4_*` oracle fixtures; diffs: 16 byte-for-byte MATCH + 1 matching
  error category (UnrecognizedImageError); see docs/compatibility.md §4/§7.
  - New crate `docxtpl-rich`: rich-text runs (all properties, five-character
    `html.escape` escaping, empty-string falsy semantics), rich-text paragraphs,
    Listing escaped text; png/jpeg/gif/bmp/tiff image-header parsing (sha1,
    pixels, dpi, extension/content-type) and EMU conversion (banker's rounding
    for single-side scaling); `wp:inline` XML matches the upstream pretty output
    character-for-character.
  - docxtpl-template: the render pipeline was generalized into typed
    `RenderContext`/`RenderValue` + the `ImageRegistry` trait; a shared shape_id
    is computed over the original document before rendering; added
    `TemplateErrorKind::Image` (oracle exception UnrecognizedImageError).
  - docxtpl-opc: python-docx-style rebuild APIs for document rels and
    `[Content_Types].xml` (order-preserving tail insertion, rId/numbering-hole
    backfill, Default/Override sorting only at serialization); added part
    appending (Deflate) and part-target relative-path resolution.
  - docxtpl-rs: new facade APIs `render_ctx(&RenderContext, ..)` and one-shot
    `render_session()`/`RenderSession::build_url_id(url)` (aligned with upstream
    `tpl.build_url_id`); image injection implements package-wide DFS baseline
    collection, sha1 deduplication, `word/media/imageN.ext` cross-extension
    numbering, and image rIds preceding anchor external links; media/rels/CT
    changes are settled once after rendering, and original bytes of untouched
    parts are preserved.
- Tightened the default OPC limits and added facade limits of 128 MiB
  compressed input, 64 MiB rendered XML, and a MiniJinja 10,000,000 fuel cap;
  exceeding them returns explicit errors.
- Pinned `time` and `deflate64` versions compatible with Rust 1.85; fixed the
  newline bytes of golden XML, fixing Windows tests affected by Git line-ending
  conversion.
- Added a LibreOffice open-and-resave CI check, the MVP usage guide, and the P1
  limit-calibration record.
- **P5: header/footer/footnote multi-part rendering (ADR-006)**. Added 7
  `p5_*` oracle fixtures; diffs: 6 byte-for-byte MATCH + 1 matching error
  category (header TemplateSyntaxError, with the error carrying the
  `word/header1.xml` part name); see docs/compatibility.md §4/§7.
  - docxtpl-template: the render pipeline is parameterized by part kind as
    Document (fix_tables/fix_docpr_ids) / Story (no fix; story-specific
    serialization: strips whitespace text outside preserve scope and keeps the
    redundant `xmlns:wp/xmlns:r` declarations of injected images) / Footnotes
    (string stage only, template declarations preserved, `NullRegistry` rejects
    footnote images = DEV-0006); images switched to lazy placeholder resolution;
    shape_id was fixed as a part-level constant (aligned with the uncached
    `StoryPart.next_id` of python-docx 1.2.0; body docPr is still renumbered by
    fix_docpr_ids starting at 1001).
  - docxtpl-xml: added `XmlDocument::strip_blank_text` (respecting
    `xml:space="preserve"` along the ancestor axis) and `serialize_story` with
    the `retain_redundant_ns` option.
  - docxtpl-rs: `render_all_parts` fixes the orchestration body → header →
    footer → core properties → footnotes; stories are enumerated through the
    main document rels (Internal targets resolved relative to the part's
    directory, non-empty, deduplicated) and footnotes by content type
    (DEV-0007: dual rels to the same target render only once; endnotes/external
    stories are unsupported); `ImageInjections` was generalized to multi-owner
    scopes (per-story rels, newly created rels mounted before write-back;
    sha1/media numbering shared package-wide; `build_url_id` external links
    always belong to the main document); `[Content_Types].xml` is normalized
    after every render (Default/Override ASCII sorting; unchanged bytes are not
    written back).
- **P6: Subdoc merging (ADR-007)**. Aligned with docxtpl 0.20.2
  `tpl.new_subdoc(docpath)` + docxcompose 2.2.0 `Composer.attach_parts`; added
  5 `p6_*` oracle fixtures (each carrying its own `*_sub.docx` subdocument);
  all 5 diffs are byte-for-byte MATCH; see docs/compatibility.md §4/§7.
  - docxtpl-template (the initial P6-stage form, since replaced at the freeze by
    `RenderValue::Subdoc(SubdocFragment)` + a RichMarkup wrapper): at the time
    it was injected as `RenderValue::Subdoc(String)` via
    `Value::from_safe_string` (aligned with `Subdoc.__html__`; emitted as-is
    whether autoescape is on or off; single-pass jinja evaluation of fragments,
    literal tags not evaluated a second time); no From/JSON entry point is
    provided.
  - docxtpl-xml: added `serialize_subtree` (declaration-free subtree
    serialization), `insert_child_at` (aligned with lxml `element.insert`), and
    `deepcopy_element` (cross-document deep copy); the main-tree serialization
    path is unchanged.
  - docxtpl-opc: added `ContentTypes::add_override` (order-preserving Override
    tail insertion; sorting happens only in to_xml).
  - docxtpl-rs: added `crates/docxtpl-rs/src/subdoc.rs` (about 1700 lines), a
    1:1 reimplementation of the attach_parts orchestration — recursive copying
    of referenced parts (partname/rId hole backfill, external-rel migration),
    three-branch style merging (name-mapping reuse / deepcopy append plus
    numbering and linked-chain fall-through rewriting), numbering copying (num
    tail-inserted before its position, anum at the first position, residual-
    mapping semantics), image merging (extension taken from the source part
    suffix, CT from the source package declaration, byte sha1 dedup), header/
    footer reference stripping, bookmark/docPr/cNvPr renumbering, and section
    guards; tree-part dirty gating preserves unchanged bytes, and image/main
    rels are settled via `ImageInjections` at finish.
  - New facade API `RenderSession::new_subdoc(path) ->
    Result<RenderValue, Error>` (callable multiple times within a session; must
    precede finish); the initial P6-stage DEV-0008 through DEV-0012 covered the
    no-docpath borrowing mode, custom.xml, nondeterministic numbering paths
    (nsid / main-missing numbering / restart actually triggered),
    SmartArt/VML/footnote references, and multi-section documents on both sides;
    the high-risk merge capabilities among them have since been closed by post-
    freeze semantic probes and integration regressions, and the current
    remaining boundaries are governed by compatibility.md.
  - On the oracle side, docxcompose==2.2.0 is locked
    (tests/oracle/requirements.txt).
- **P7: the media/embedded replacement family and template introspection
  (ADR-008)**. Aligned with docxtpl 0.20.2
  `replace_media`/`replace_embedded`/`replace_zipname`/`replace_pic`/
  `reset_replacements` and `get_undeclared_template_variables`; added 7
  `p7_*` oracle fixtures (including 1 `skip_render` save-without-rendering case
  and 8 fixed-byte replacement assets); diffs: 6 byte-for-byte MATCH + 1
  matching error category (ValueError); the render-fixture baseline reached 85
  (81 MATCH + 4 error categories), golden 85/83; see
  docs/compatibility.md §4/§7.
  - docxtpl-template: added the public function
    `find_undeclared_variables(doc_xml, story_xmls) ->
    Result<BTreeSet<String>, _>` (body and stories are patched separately and
    then concatenated; minijinja meta-analysis; loop variables excluded
    automatically); added `TemplateErrorKind::InvalidArgument` (oracle exception
    ValueError).
  - docxtpl-rs: added `crates/docxtpl-rs/src/replacements.rs`; `Replacements`
    has four registration tables (media/embedded keyed by CRC32, zipname by
    exact full name, pics as an order-preserving Vec aligned with upstream dict
    insertion order / break on first hit); the pre path `apply_pic_replacements`
    (header/footer targets in appearance order in the main document and main
    rels, without dedup; pic:graphicData only; structurally missing cases are
    skipped wholesale; missing identifiers raise ValueError) and the post path
    `apply_byte_replacements` (exact zipname > media CRC > embeddings CRC; only
    the blob is swapped); new facade APIs
    `RenderSession::replace_media/replace_embedded/replace_zipname/
    replace_pic/reset_replacements` (chainable),
    `RenderSession::finish_without_render()` (save directly without rendering;
    fix_tables/fix_docpr_ids do not run), and
    `DocxTemplate::undeclared_variables()`; DEV-0013 (state at the P7 freeze):
    introspection offers no context diff or custom jinja_env, and
    allow_missing_pics is always False; the context diff and lenient image
    policy among the first two have since been provided in post-freeze
    hardening, while the Python `jinja_env` boundary remains.
  - New dependency `crc32fast = "1.5"` (the same IEEE polynomial as Python
    `binascii.crc32`; pinned by fixture diffs).
- **P7b: byte alignment with a real-Word template corpus from docxtpl 0.20.2
  (ADR-009)**.
  Introduced 16 real-Word templates from the upstream repository (LGPL-2.1) as
  `p7b_*` render fixtures (4 of them with python contexts), closing 5 of the 6
  blocking difference families (B6 RichText/eastAsia was supported with zero
  changes); the render-fixture baseline reached 101 (97 MATCH + 4 error
  categories), golden 101/99, with zero regressions across the existing 85; see
  docs/compatibility.md §4/§7.
  - docxtpl-template: added public `normalize_part_xml(src, part_name)`
    (strict parsing + strip_blank_text + an always-single-quoted lxml
    declaration); body/header/footer patch inputs go through an oxml tree
    round-trip (entity decoding, indentation stripped); the footnotes path does
    not round-trip the tree (the generic Part's original bytes pass through,
    preserving Word's double-quoted declarations); the render-string entry point
    uniformly normalizes CRLF/CR to LF (aligned with the Jinja2 lexer tnewline);
    fixed the typo in `{_% %_}` literal escape restoration (`{_%`→`{%`).
  - docxtpl-opc: added `ContentTypes::rebuild_from_parts` (ported the
    python-docx spec.py default content-type table; rels/xml Default entries are
    always present and rels Override entries disappear), plus
    `Package::rebuild_content_types` and `Package::normalize_relationships`
    (root and mounted rels all rewritten to canonical bytes; unchanged bytes are
    not written back).
  - docxtpl-rs: pre-save normalization was upgraded to three steps — known
    XmlPart tree round-trip for styles/settings/numbering (generic Part blobs
    passed through) → rels normalization → CT rebuild from_parts; shared by
    render() and RenderSession::finish.
  - docxtpl-xml: the body serialization path simulates lxml's namespace merging
    when reattaching across trees (when an element-local xmlns has the same URI
    as an ancestor's, the declaration is dropped and element/attribute/
    descendant prefixes are rebound to ancestor prefixes; whole-tree
    parse/tostring paths for headers and footers stay lexical, ADR-006).
  - tests: `generate.py` gains the `source`/`source_upstream` mechanism (real
    templates are copied from `tests/fixtures/sources/` rather than built
    programmatically; the manifest records the upstream provenance); oracle_diff
    gains 4 P7b Rust python-context arms; per-part byte/c14n diffing is primary,
    and representative on-machine Microsoft Word and LibreOffice spot checks
    were added later in P7d.

## [0.1.0-alpha]

The first MVP: the full P0–P3 scope was completed, with all-green oracle diffs
against Python docxtpl 0.20.2 (49 render fixtures: 48 byte-for-byte MATCH + 1
matching error category; plus 20 real-docx OPC byte round-trips, 49 patch
goldens, and 48 recover goldens; see docs/compatibility.md §7).

- docxtpl-opc: ZIP/OPC package reading and writing, `[Content_Types].xml` and
  relationship indexes, main-document locating, package validation, input
  limits (entry count/size/compression ratio/output cap), and original-byte
  preservation of unmodified parts.
- docxtpl-xml: strict and lenient (libxml2 recover-style) parsing, a Vec-arena
  editable tree, and preserving serialization aligned byte-for-byte with lxml
  (single-quoted XML declarations, etc.).
- docxtpl-compat: a line-by-line port of the 13-step `patch_xml` regex
  transforms and of `resolve_listing` (`\n \t \u07 \u0c`
  → br/tab/paragraph break/page break).
- docxtpl-template: the main body render pipeline (patch → insert/remove
  newlines → MiniJinja (lenient undefined, autoescape off by default) → literal
  escape restoration → resolve_listing → recover → fix_tables → fix_docpr_ids
  → lxml-style serialization); structural p/tr/tc/r tags,
  colspan/cellbg/vm/hm, table-grid add/remove correction, and wp:docPr
  renumbering from 1001 (id attributes without the prefix).
- render_properties: on every render, unconditionally renders the 6 string core
  properties (author/comments/identifier/language/subject/title); missing
  dc:identifier and dc:language elements are filled in per upstream setattr side
  effects.
- docxtpl-rs: facade APIs `DocxTemplate::open/from_reader/from_bytes`,
  `render(&json, &RenderOptions)`, `RenderedDocument::save/write_to/to_bytes`;
  stable error categories `TemplateErrorKind::{Syntax,Undefined,Other}` and
  docx_context line context aligned with upstream.
- docxtpl-cli: `docxtpl render <template> <context.json> <output>
  [--autoescape]`.
- Security: DTD/external entities forbidden; package and XML depth/size limits
  (ADR-004).
