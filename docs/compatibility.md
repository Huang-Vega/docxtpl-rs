# docxtpl-rs compatibility list

> One of the single sources of truth for behavior PRs (code spec §1.3). Each
> feature is registered as compatible / documented-deviation / unsupported and
> must have a minimal fixture or a live semantic probe.
> Status markers: ✔ differential passes · ◐ partial support / deviation · ✘ unsupported · ⏳ planned

## 1. Reference implementation fingerprint (fixed at P0, see ADR-001)

| Item | Value |
|---|---|
| Upstream | python-docx-template (docxtpl) **0.20.2** |
| PyPI | `docxtpl==0.20.2`, released 2025-11-13 |
| Git | tag `v0.20.2`, SHA `cf5437bdf5d30f9362149ddea508d6d9f008b6cd` |
| Subdoc merge dependency | docxcompose **2.2.0** (oracle side; docxtpl `Subdoc` merges parts via its `Composer.attach_parts`, P6) |
| License | LGPL-2.1-only (workspace and pinned upstream baseline; see ADR-001) |
| Dependency locks | `tests/oracle/requirements.txt` + `Cargo.lock` |

## 2. Upstream rendering pipeline (audit conclusions)

Audit source: `third_party/docxtpl-0.20.2/docxtpl/template.py` (not checked in;
see ADR-001 for how to obtain it).

1. `get_xml()`: serializes only `w:body` (lxml attaches xmlns declarations
   visible on the root to the output).
2. `patch_xml()`: 13 **bounded** string regex transforms (see §3 for details;
   the table below merges them into 11 semantic groups A–K).
3. `render_xml_part()`: insert `\n` before each `<w:p[ >]` (for line-number
   location only) → `jinja2.Template(src).render(context)`
   → remove `\n<w:p` → restore `{_{ }_} {_% %_}` literal escapes →
   `resolve_listing()`.
4. `render()` (default `autoescape=False`): body → `fix_tables()`
   (lenient parsing via `etree.fromstring(xml, parser=XMLParser(recover=True))`
   + tblGrid column-count fix) → `fix_docpr_ids()`
   (renumber `wp:docPr/@id` inside the body sequentially from 1001) → graft
   back into `w:document`.
   Then render headers/footers, core properties (only the 6 string properties,
   no patch), and footnotes.
5. Values are inserted into XML as-is; invalid XML relies on libxml2 recover
   lenient parsing to heal (bare `&` is kept as text, `<b>`-style injections
   become real elements), followed by lxml re-serialization (ADR-002).

## 3. patch_xml steps and porting status (docxtpl-compat)

| # | Semantics | Upstream essentials | Phase/status |
|---|---|---|---|
| A | Delimiter split healing | XML markup between `{` and `{`/`%`/`#` deleted (works across runs/paragraphs) | P2 ✅ |
| B | Cross-run merge inside tags | `</w:t>…<w:t>` boundaries inside tags deleted | P2 ✅ |
| C | `{% colspan expr %}` | in the hosting `w:tc`, empty the run and delete the first gridSpan; inject `w:gridSpan w:val="{{expr}}"` after `w:tcPr` | P3 ✅ |
| D | `{% cellbg expr %}` | as above, inject `w:shd w:fill="{{expr}}"` | P3 ✅ |
| E | add `xml:space="preserve"` to a `w:t` containing tags | applies only to bare `<w:t>` (no attributes) | P2 ✅ |
| F | `{{r …}}` / `{%r …%}` become their own run | split out unformatted new runs before and after | P3 ✅ |
| G | `{%(p\|tr\|tc\|r) …%}` / `{{(p\|tr\|tc\|r) …}}` structural tag lifting | a regex with nesting guards replaces the **entire hosting element** with an ordinary tag; order: tr, tc, p, then r | P3 ✅ |
| H | `{#(p\|tr\|tc) …#}` comment lifting | same as G, content contains no `}`/`#` | P3 ✅ |
| I | `{% vm %}` vertical merge | inject `w:vMerge` restart/continue at the end of tcPr; text kept only in the first iteration | P3 ✅ |
| J | `{% hm %}` horizontal merge | multiply the gridSpan value by `loop.length` (add it if absent); the whole cell kept only in the first iteration | P3 ✅ |
| K | clean_tags | `&#8216;`/`&lt;`/`&gt;` and smart quotes (“ ” ‘ ’) inside tags restored to ASCII | P2 ✅ |

## 4. Feature support matrix

### P2/P3 (MVP, 0.1.0-alpha)

| Feature | Level | Fixture category |
|---|---|---|
| Plain variables / multiple variables / whitespace tolerance | compatible target | r2_var_* |
| Jinja built-in filters/tests (including upper/lower/string/join/replace/format/default/map/selectattr/sort/sum) | success paths in the table are compatible; dedicated coverage exists for `join(attribute=...)`, container/float Python spellings, and MarkupSafe paths. VM comparison, modulo, and type-test boundaries are in DEV-0018; this is not equivalent to the full Jinja2 built-in surface | r2_filter_* + template-crate compatibility-hardening regressions |
| Common Python methods on JSON dict/list/string and deterministic Jinja items (items/keys/values/get, split/startswith/endswith/count/index, center/filesizeformat/striptags/truncate/wordcount/wordwrap, cycler/joiner/callable) | success/ValueError paths in the live table are compatible; `striptags` decodes the full HTML5 named-entity set | `jinja_live_diff` + template-crate compatibility-hardening regressions |
| Rust-native MiniJinja filter/test/function injection | at the implementation layer, body, header/footer, core properties, and footnotes share one configuration; Python Jinja2 Environment/extension objects are not accepted | `p5_core_boundaries::rust_environment_configuration_*` directly covers document/core and the two high-level entry points; other parts reuse the same orchestration |
| Unicode identifiers and case handling; JSON/object preserves input insertion order | compatible target | template-crate unicode / preserve_order regressions |
| JSON integers | context conversion and direct output/`tojson` preserve values exactly within signed i128 range; out-of-range values return an explicit `InvalidArgument`, with no f64 downgrade; full Python bigint arithmetic semantics are not promised | template-crate arbitrary-precision regression; DEV-0016/0018 |
| `DocxTemplate::render` and the public JSON render functions require a top-level object; array/scalar/null explicitly return the `InvalidArgument` / `ValueError` categories | compatible target; the legacy `RenderContext::from_json` still keeps non-object → empty context; for untrusted input use `try_from_json` | p7_regressions::public_json_render_rejects_non_object_contexts |
| `tojson` / `urlencode` / `escape` (`e`) / `forceescape` / `xmlattr` | compatible target (Jinja2 3.1.6 default JSON, URL, and MarkupSafe escaping and error categories) | template-crate compatibility-hardening regression |
| undefined leniency (rendered as empty string) | compatible target | r2_undefined |
| if/else, for, comments, set | compatible target | r2_if_* / r2_for_* / r2_comment / r3_p_set |
| Whitespace control `{%- -%}` | compatible target | r2_trim |
| recover healing of special characters in values (`&`/`>`/quotes/`<`) | compatible target (behavior pinned by the oracle) | r2_value_* |
| `\n`/`\t` inside values (resolve_listing) | compatible target | r2_newline/tab_value |
| Split-tag merging (steps A/B) | compatible target | r2_split_* |
| Literal `{_{ }_}` escapes | compatible target | r2_literal_escape |
| p/tr/tc/r structural tags (including empty/single/multi loops and nesting) | compatible target | r3_p_* / r3_tr_* / r3_tc_* |
| colspan / cellbg / vm / hm | compatible target | r3_colspan / cellbg / vm / hm |
| fix_tables grid add/remove correction | compatible target; extreme `gridSpan` counting uses saturating arithmetic to avoid integer overflow | r3_fix_add / r3_fix_remove + extreme-value regression |
| fix_docpr_ids renumbering | compatible target | r3_docpr |
| render_properties core properties (Jinja rendering of the 6 string properties; missing dc:identifier/dc:language elements are filled in) | compatible target (runs unconditionally on every render) | byte-for-byte coverage across all 48 success fixtures |
| Error-category alignment for render errors (syntax etc.) | compatible target | r2_syntax_error |

### P4 (RichText and images, 0.8.0 increment, ADR-005)

| Feature | Level | Fixture category |
|---|---|---|
| RichText run properties (bold/italic/u/strike/color/size/highlight/style/font regional syntax/sup/sub/rtl/lang), multi-run concatenation, and empty-value semantics | compatible target | p4_rt_basic / p4_rt_style_font / p4_rt_in_table |
| RichText external hyperlinks (`tpl.build_url_id` pre-registers an external rel → `w:hyperlink r:id`) | compatible target | p4_rt_url |
| RichTextParagraph (with/without parastyle; rich text in a paragraph; `with_text("")` is an empty fragment, explicit `add("")` produces an empty `<w:p>`) | compatible target | p4_rtp_basic + rich-crate empty-value regression |
| Listing (`\n \t \a \f` expanded via resolve_listing; mixed with RichText) | compatible target | p4_listing_basic / p4_listing_after_rt / p4_combo_rich |
| InlineImage native size (png/jpg/bmp/gif/tiff header parsing, EMU conversion; for JPEG, preserve the APP0/APP1 DPI path selected by the JFIF/Exif signature at offset 6, with a forward-compatible safe SOI/SOF fallback at 72 dpi when neither signature exists) | compatible target + forward extension for marker-valid camera JPEGs | p4_img_png / p4_img_wh / p4_img_formats + JFIF/Exif/generic marker regressions |
| InlineImage single-side scaling (aspect-ratio banker's rounding) | compatible target | p4_img_scale_w |
| Image sha1 deduplication (identical bytes reuse the part and rId) | compatible target | p4_img_dup / p4_img_in_table / p4_combo_rich |
| Multiple distinct images (imageN numbering / rId hole backfill) | compatible target | p4_img_two / p4_img_formats |
| Image hyperlink anchors (the image rId comes before the anchor external rId; an empty anchor creates no hyperlink relationship) | compatible target | p4_img_anchor + images empty-anchor regression |
| The InlineImage file basename written into `pic:cNvPr/@name` is escaped per XML attribute rules; filename/title/descr pre-check XML 1.0 characters and an aggregated 64 MiB budget after escaping | compatible + security hardening; invalid/oversized metadata return dedicated new public `ImageError` variants | rich-crate filename, invalid-character, and budget regressions |
| InlineImage `title`/`descr` written into both `wp:docPr` and `pic:cNvPr` | **forward extension**: comes from docxtpl master, not part of the 0.20.2 compatibility denominator | rich-crate accessibility regression |
| Images inside table-row loops (repeated resolution of the same value is idempotent) | compatible target | p4_img_in_table |
| media part injection plus python-docx-style rebuild of document rels and [Content_Types].xml | compatible target (byte-for-byte, DEV-0003 not triggered) | all image-bearing p4 fixtures |
| Bad-image error-category alignment (UnrecognizedImageError; the probe runs before any part/rId allocation) | compatible target | p4_img_bad |
| Under `autoescape=True`, RichText / Listing / InlineImage are injected as-is via `__html__` safe-value | compatible target (byte-for-byte) | p4_autoescape_rich |

### P5 (header/footer/footnotes, 0.8.0 increment, ADR-006)

| Feature | Level | Fixture category |
|---|---|---|
| Header/footer Jinja rendering (variables, if, `{%p for%}` paragraph loops; each section has its own header/footer; untagged stories round-trip as-is) | compatible target (lxml round-trip, resolve_listing still runs, no fix_tables/fix_docpr_ids) | p5_hf_basic / p5_hf_multi / p5_hf_untagged |
| Header/footer RichText / Listing (external-hyperlink build_url_id acts on the main document rels) | compatible target | p5_hf_richtext |
| Header/footer InlineImage (multiple images, anchor hyperlinks; rId/media allocated with per-part scope, sha1 package-level dedup; docPr part-level constant id/name; compact story serialization and redundant xmlns preserved) | compatible target (byte-for-byte) | p5_hf_image |
| Footnote part string rendering (keeps the template XML declaration and unchanged bytes; RichText/Listing go through the same pipeline) | compatible target | p5_footnotes_basic |
| Endnote part string rendering | forward extension: marker-bearing endnotes use the same raw UTF-8 generic-part path as footnotes; untagged endnotes remain byte-for-byte untouched like upstream | p5_story_boundaries package-level regression + real-Word oracle corpus |
| XmlPart input encoding (body/header/footer/core accept UTF-8, UTF-16LE/BE, and UTF-32LE/BE by BOM or the XML initial-byte pattern; saved uniformly as UTF-8; unsupported encoding declarations error explicitly) | semantic-compatible; current direct evidence is Rust package-level regressions, not counted in the frozen Python oracle; the generic footnotes Part still accepts only UTF-8 per upstream `blob.decode()` | p5_xml_encoding package-level regression |
| Story syntax errors carry the concrete part name (error category aligned with TemplateSyntaxError) | compatible target | p5_hf_syntax_error |
| Normalization of `[Content_Types].xml` after rendering (Default sorted by extension, Override by part name) | compatible target | evidenced by p5_footnotes_basic |
| DEV-0007 boundaries (exact match on official story relationship URIs, dedup of dual rels to the same target, External/orphan stories not rendered; footnotes/endnotes rendered only once per part) | documented-deviation regression + forward extension | p5_story_boundaries package-level variants |

### P6 (Subdoc merging, 0.8.0 increment, ADR-007)

| Feature | Level | Fixture category |
|---|---|---|
| External docx Subdoc fragment injection (`new_subdoc(path)` and the equivalent `new_subdoc_from_reader/from_bytes`; merged at construction with `{{p sd }}`; single-pass jinja evaluation of fragments, literal tag text output as-is; empty-body fragments) | compatible target (byte-for-byte) | p6_subdoc_basic / p6_subdoc_verbatim / p6_subdoc_untagged + reader/bytes entry-point regressions |
| Three-branch style merging (sub id→name→main id mapping reuse; deepcopy append of styles missing in the main document + numbering/linked-style chains; reference fall-through rewriting; styles.xml dirty gating) | compatible target (byte-for-byte) | p6_subdoc_style |
| Subdoc image merging (extension taken from the source part suffix, CT from the source package declaration, byte sha1 dedup; media/main rels/CT finalized via ImageInjections at finish) and external hyperlink relationship migration | compatible target (byte-for-byte) | p6_subdoc_image |
| Recursive copy of referenced parts (partname/rId hole backfill), bookmark/docPr/cNvPr renumbering, stripping of header/footer references, section guards (all corpus inputs take the no-op path, pinned by the byte baseline) | compatible target | all p6 fixtures |
| When the main package lacks `word/numbering.xml`, create the part/relationship/CT; generate a unique uppercase 8-hex-digit `w:nsid` when copying numbering; restart numbering for the first non-heading/non-bullet list | semantic-compatible; `w:nsid` is a deterministic value, see DEV-0010 | Subdoc numbering integration regression + Python oracle semantic probe |
| Fix the main document section-start type when both main and sub documents contain multiple sections | compatible target | Subdoc multi-section integration regression + Python oracle semantic probe |
| Subdoc fragments are parsed with the in-scope namespaces of the main `w:body`, supporting fragments that rely on inherited prefixes such as `m`/`w14`/`wp14` | compatible target | Subdoc namespace integration regression |
| VML `v:shape/v:imagedata` image migration and SmartArt `dgm:relIds` four-category relationship/part copying plus repeated-reference reuse | semantic-compatible | Subdoc high-risk integration regression + Python oracle semantic probe |
| Custom properties: discover `docProps/custom.xml` via package-root relationships, dissolve same-named simple/complex `DOCPROPERTY` fields, do not copy the custom part | semantic-compatible | Subdoc high-risk integration regression + Python oracle semantic probe |
| Footnote references: create or merge the footnotes part/relationship/CT on demand, renumber footnote ids, and migrate relationships internal to footnotes | semantic-compatible; relationship rewriting fixes an upstream dangling-rId risk, see DEV-0011 | Subdoc high-risk integration regression + Python oracle semantic probe |

### P7 (media/embedded replacement family and template introspection, 0.8.0 increment, ADR-008)

| Feature | Level | Fixture category |
|---|---|---|
| `replace_media` replaces `word/media/` entries by source-byte CRC32 (the post path swaps only the blob; the same media part referenced from a header is hit globally) | compatible target (byte-for-byte) | p7_media_body / p7_media_header |
| `replace_pic` replaces image blobs identified by cNvPr name/title/descr (main document + header/footer targets in appearance order in the main rels; pic:graphicData only; multiple references within the same part count once; registration insertion order is matched, break on hit) | compatible target (byte-for-byte) | p7_pic_match |
| All `replace_pic` identifiers miss → ValueError (default `allow_missing_pics=false`); after the lenient switch is explicitly enabled, misses are silently ignored, and the policy is not cleared by `reset_replacements` | compatible target | p7_pic_missing + P7 compatibility-hardening regression |
| `replace_embedded` (`word/embeddings/` CRC) + `replace_zipname` (exact full zip entry name, leading `/` stripped; higher priority than CRC) | compatible target (byte-for-byte) | p7_embedded_zipname |
| Save without rendering (`finish_without_render`: no patch/render/fix_tables/fix_docpr_ids, docPr semantics preserved; pre/post replacements run as usual; at save time python-docx-known XmlParts are still re-serialized and all rels/CT normalized) | compatible target; original fixtures byte-for-byte; real-Word XmlParts have additional morphology regressions | p7_replace_only (skip_render) + P7 compatibility-hardening regression |
| `reset_replacements` clears the four registration tables | compatible target | covered by library unit tests / session paths |
| `get_undeclared_template_variables` template introspection (bare jinja meta-analysis after patching the body + all headers/footers; loop variables excluded automatically; can diff against a typed context or a key set) | compatible target (BTreeSet ordering equivalent to Python sorted) | p7_undeclared_vars + P7 compatibility-hardening regression |
| `get_pic_map` image-name introspection (body and header/footer, cNvPr name → relationship relative target) | compatible API | p7_regressions::picture_map_reports_relative_target |

### P7b (real-Word template corpus from docxtpl 0.20.2, ADR-009)

Corpus: 16 real-Word templates from the upstream repository's
`tests/templates/` (LGPL-2.1, copied into `tests/fixtures/sources/`, fixture
ids 106-121). The validation target is not new syntax but byte-for-byte
equivalence under real Word 2016 save morphology (double-quoted declarations +
CRLF, inter-element indentation, local namespaces, rels Override,
customXml/footnotes/comments parts).

| Feature/difference family | Level | Fixture category |
|---|---|---|
| Real-Word structure rendering (if/for/nested tables, `|count` filter, run splitting, `{_%-`/`{%-` whitespace control, spaces around tags preserved, vm/hm/inline literal list, fix_tables cell deletion) | compatible target (byte-for-byte) | p7b_order / p7b_dynamic_table / p7b_merge_paragraph / p7b_preserve_spaces / p7b_vm / p7b_hm / p7b_less_cells |
| B2: oxml tree round-trip of patch input (remove_blank_text, entity decoding; `&quot;`/`&apos;` inside header/footer expressions, `{#tr/tc#}` comment deletion) | compatible target (byte-for-byte) | p7b_hf_entities / p7b_comments |
| B6: real-Word RichText (pure-space/Tab values, RichText inside `{%p if%}` paragraphs, `eastAsia:` font prefix rFonts w:eastAsia, cellbg row RichText cells) | compatible target (byte-for-byte, zero changes) | p7b_word2016 / p7b_richtext_if / p7b_eastasia / p7b_cellbg (all python contexts) |
| B1: package-level normalization at save time (CT rebuilt from_parts: rels Override disappears / rels+xml Default always present / spec default table lands in Default; all rels including customXml child rels rewritten; styles/settings/numbering known XmlParts always re-serialized by lxml, generic Part blobs passed through) | compatible target (byte-for-byte) | all 16 p7b (typically p7b_nested_for's customXml/item1.xml and item1.xml.rels) |
| B4: namespace merging when reattaching across trees (paragraph-local `xmlns:wp14` and root `xmlns:w14` with the same URI → declaration dropped, element/attribute prefixes rebound to ancestor prefixes; header/footer that are pure parse/tostring are not merged) | compatible target (byte-for-byte) | p7b_vm_nested |
| B5: generic footnotes Part original-byte round-trip (untagged/tagged footnotes keep the template's double-quoted declarations; the jinja lexer tnewline normalizes CRLF/CR to LF) | compatible target (byte-for-byte) | p7b_footnotes_real (tagged); untagged footnotes pass through p7b_comments / p7b_nested_for / p7b_eastasia |
| B3: `{_% %_}` literal escape restoration (`{_%`→`{%`, fixing the historical typo `{%_`) | compatible target (byte-for-byte) | p7b literal-escape cases such as p7b_merge_paragraph |

### P7c/P7d (compatibility hardening and the 0.8.x freeze, ADR-010)

| Item | Level | Acceptance |
|---|---|---|
| No panic on random/truncated ZIP, arbitrary XML, or marker-heavy patch input | compatibility hardening | 6 proptest properties; 256/512 cases each |
| Default resource limits leave headroom for the frozen corpus | compatibility hardening | 127 templates; max 23 entries, 833 014 B total decompressed, 32.156 compression ratio |
| Staged release performance regression baseline | 0.8.x candidate gate | 3 tiered samples, 15 runs each; split open/render/write and record peak RSS; a same-machine `--compare` render regression >20% blocks; not yet wired into a stable same-machine CI runner |
| Windows/Linux/macOS basic regression | candidate gate | fmt/clippy/test/oracle measured on Windows and Ubuntu 24.04 x86_64; Linux LibreOffice 24.2.7.2 opens and resaves 8/8; macOS 26.6.2 arm64 measured fmt/clippy/test/oracle/MSRV, with performance and LibreOffice 8/8 passing |
| Microsoft Word manual visual spot check | candidate gate | Windows 11 Pro x64 / Word 16.0.17932.20700 x64; representative Rust outputs open/save/reopen 10/10; manual inspection of Word-exported pages 11/11 |

### Follow-up compatibility-hardening regressions (not counted in the frozen fixture count)

What this round adds is crate unit tests, dynamic DOCX integration tests, and
standalone Python oracle **semantic probes**; they do not change the manifest
counts in §6, nor do they claim per-part byte acceptance for the high-risk
Subdoc paths based on existing fixtures. The new `jinja_live_diff` also
compares table-driven expressions live against the currently locked Python
Jinja2 under the oracle feature, avoiding Rust-only self-validated
expectations. Coverage includes:
Jinja preserve-order/unicode, lossless float parsing, common Python object
methods, deterministic filters/globals/tests, Python-style stringification and
MarkupSafe details, JPEG JFIF/Exif DPI and image-filename escaping, Subdoc
numbering/sections/namespaces/VML/SmartArt/custom-properties/footnotes, as well
as introspection context diffs, `allow_missing_pics`, and save-time
normalization of real-Word XmlParts under skip-render. Security hardening also
covers allocation-time bounded XML serialization, layered `resolve_listing`
expansion, streaming output of expansion-type filters, and intermediate-item
budgets; these are per-path budgets, not a process-wide 600 MiB RSS cap.

### unsupported (explicitly rejected)

- Passing in Python `jinja_env` / Jinja2 extension objects, line statements,
  arbitrary Python objects and callables; Rust callers can register MiniJinja
  filters/tests/functions via `RenderOptions::with_environment_configurator`,
  but that configurator affects rendering only and does not participate in
  static template introspection.
- The default environment provides no template loader, so
  `include`/`import`/`extends` are not promised; the `urlize` filter also
  remains outside the compatibility surface (DEV-0019).
- The Subdoc current-document borrowing mode without `docpath`, and
  constructing Subdoc values from a JSON context.
- The low-level `SubdocFragment` only validates/carries an already-merged XML
  fragment; it does not automatically migrate relationships, styles, or media;
  regular callers should use `RenderSession::new_subdoc`.
- `{%p%}`-style tag content containing `%` or `}` (the upstream regex does not
  match, it falls through to ordinary text and triggers a jinja syntax error; we
  keep the same error).
- DTD / external entities (security-side deviation, ADR-004).

## 5. documented-deviation register

| ID | Description | Basis |
|---|---|---|
| DEV-0001 | Input/output/fuel limits are a security enhancement absent upstream; P8 large-document mode raises the default document and XML budgets to 600 MiB and the ZIP entry limit to 6000, and adds `ResourceLimits` configuration. XML allocation-time serialization, layered listing expansion, image replacement, and expansion-type filter output remain bounded; intermediate collections are additionally subject to a 4 915 200-item budget. These per-path budgets are not equivalent to a process-wide 600 MiB memory cap. The CLI JSON file remains 64 MiB; programmatic contexts have no equivalent input-byte limit, and `tojson` still materializes an intermediate value tree. All frozen corpora are within the limits | ADR-004 / ADR-011 / security-limits.md |
| DEV-0002 | The long-tail behavior of libxml2 recover is pinned only by the corpus; unpinned inputs are handled conservatively and recorded as diagnostics | ADR-002 |
| DEV-0003 | ZIP timestamps/entry order and the **entry order** of `[Content_Types].xml` and rels are treated as non-semantic differences, normalized by canonicalization | planning document §9.5 |
| DEV-0004 | python-docx-registered XmlParts are re-serialized at save time per upstream; other unmodified generic Parts keep their bytes as far as possible (semantic equivalence proven via c14n/dedicated regressions) | ADR-002 / ADR-009 |
| DEV-0006 | InlineImage in footnotes (and any non-story generic Part) is unsupported: upstream calls `new_pic_inline` on a binary Part with no registered PartFactory and raises `AttributeError`; our `render_footnotes_xml_ctx` returns an error carrying the part name at placeholder resolution via `NullRegistry`, pinned by a template-crate unit test | ADR-006 |
| DEV-0007 | Stories are recognized only by the full official header/footer relationship URI and only Internal targets are scanned; external/orphan stories are not rendered; when the same part is referenced by multiple relationships it is rendered only once (dedup by target). Marker-bearing endnotes are a forward extension and render once through the generic note-part path; untagged endnotes remain untouched. Upstream `render_footnotes` traverses the same footnotes part repeatedly per section; we render each note part once to avoid previous output being treated as a template again | ADR-006 |
| DEV-0008 | Subdoc supports external docx paths, `Read + Seek` streams, and byte forms; the upstream borrowing mode without docpath (`Subdoc` directly reusing the current document's parts) is still not provided as API; Subdoc values cannot be passed via a JSON context; fragment-equivalence regressions for the three external inputs and the no-argument compile-fail regression lock this boundary | ADR-007 |
| DEV-0010 | Upstream generates `w:nsid` with `random.random()`, making output nondeterministic for the same input; we instead use a deterministic unique uppercase 8-hex-digit value based on the copy context. Numbering structure, missing-numbering-part creation, and restart behavior are aligned via semantic probes, but the random bytes are not followed | ADR-007 |
| DEV-0011 | When copying footnotes, we write the new rIds of footnote-internal relationships into the clone actually appended to the main footnotes; this intentionally fixes a docxcompose 2.2.0 issue that could leave dangling references when the target already has the relationship and rIds must be reallocated. Ordinary conflict-free inputs are semantically identical to the oracle; replicating that upstream defect is not promised | ADR-007 |
| DEV-0013 | Introspection context diffs and `allow_missing_pics=True` are provided; the Rust-native `RenderOptions::with_environment_configurator` can inject MiniJinja filters/tests/functions into all parts of the same render, but does not participate in static introspection. Python `jinja_env` / Jinja2 extension objects still cannot be injected directly | ADR-003 / ADR-008 |
| DEV-0014 | The direct string-enumeration entry point has been replaced by an opaque `SubdocFragment` subject to strict XML/DTD/namespace validation; it can be parsed with the main body's namespace context, but this low-level entry still cannot merge relationships/styles/media automatically and should only handle already-merged fragments; regular calls must use `RenderSession::new_subdoc` | ADR-010 |
| DEV-0015 | The MiniJinja VM's `~` operator uses the internal `Value::Display` directly before the environment formatter, and the value layer does not preserve the original types of Python tuples/ranges/bytes; ordinary final output and `string`/`join`/`replace`/common positional `%s` have been narrowed, but byte-for-byte compatibility is not promised for the spellings of those types, printf mapping, and `%r/%a` | ADR-003 |
| DEV-0016 | When Python arbitrary-precision integers exceed signed i128, we return `InvalidArgument` explicitly. This avoids MiniJinja's silent overflow on oversized U128 arithmetic; there is no f64 conversion and no claim of full Python bigint compatibility | ADR-003 / ADR-004 |
| DEV-0017 | InlineImage's deferred placeholder token uses an independently randomized namespace per context conversion, avoiding accidental collisions between ordinary input and the old fixed token; it is not described as cryptographically unpredictable or unforgeable. Direct image output already has compatibility evidence; the full Python object semantics through transform-type filters such as `string`/`forceescape`/`replace`/`striptags`/`urlencode`/`wordcount` are still unimplemented and may leave deformed tokens or return different results | ADR-003 / ADR-005 |
| DEV-0018 | The MiniJinja VM/type model still differs from Python: modulo with a negative divisor, heterogeneous-type comparison/error behavior, and `True`/`1` key identity in generic mappings may differ; `bool is number` and `is sequence` for strings/mappings are now aligned. The specialized key normalization in `tojson` does not mean generic mapping semantics are aligned | ADR-003; live Python gate in `jinja_live_diff` plus source audit/manual probes against Jinja2 3.1.6 |
| DEV-0019 | The default Jinja library surface is not fully implemented: `urlize` and a default template loader remain absent. `striptags` now uses the full HTML5 entity set, while `random`, `lipsum`, and `randrange` are registered. Only the success paths explicitly listed in §4/`jinja_live_diff` are part of the compatibility promise | ADR-003; live Python differential plus template-crate registration/invariant regressions |

DEV-0005, DEV-0009, and DEV-0012 have been closed by later implementations, so
their numbering holes are retained and the IDs are not reused.

## 6. Fixture baseline

See `tests/fixtures/manifest.json` (P0: 20 round-trips; P2/P3: 48 success +
1 expected error = 49; P4: 17 success + 1 expected error = 18;
P5: 6 success + 1 expected error = 7; P6: 5 success = 5
(each additionally carrying a `templates/<id>_sub.docx` subdoc); P7:
6 success + 1 expected error = 7 (of which p7_replace_only is marked
`skip_render: true`, plus 8 `media/p7_*` replacement assets); P7b:
16 success = 16 (real upstream templates from docxtpl 0.20.2, `source` copied
from `sources/p7b_*.docx` without building, of which 4 have
context_kind="python"); there are currently 102 `mode=render` entries in total,
whose total count and per-phase coverage are verified directly from the
manifest by the oracle tests; all are annotated with
id/feature/phase/mode/context_kind/expected/owner).
The measured differential results are in §7.

## 7. Differential results (measured at 0.8.0)

Execution environment: Windows + rustc/cargo 1.95.0; oracle = Python docxtpl
0.20.2 / Jinja2 3.1.6 / python-docx 1.2.0 / lxml 6.1.1 (see §1).
The frozen 324 denominator is reproduced by the following commands:

```sh
cargo test -p docxtpl-rs --features oracle --test oracle_diff
cargo test -p docxtpl-opc --test fixture_roundtrip
cargo test -p docxtpl-compat --test golden_patch
cargo test -p docxtpl-xml --test golden_recovery
```

The post-freeze dynamic Jinja regressions run separately and are not counted in
the 324:

```sh
cargo test -p docxtpl-rs --features oracle --test jinja_live_diff
DOCXTPL_PYTHON_ORACLE=python cargo test -p docxtpl-template live_python_oracle_for_new_compat_surface
```

The second line uses a POSIX shell environment-variable prefix; in PowerShell,
first run `$env:DOCXTPL_PYTHON_ORACLE = "python"`, then run the corresponding
`cargo test`.

Judgment criteria (tests/oracle/compare.py): identical part-name sets +
**identical raw-byte sha256 for every part** + identical
`[Content_Types].xml`/rels sets + identical exclusive C14N sha256 for every
XML part (with docProps/core.xml timestamp normalization).

| Category | Case count | Result |
|---|---|---|
| r2_* rendering (variables/filters/undefined/if/for/trim/comments/splits/literal escapes/special-character values/listing/smart quotes/entities/empty loops) | 26 | 26 MATCH |
| r2_syntax_error (jinja2 TemplateSyntaxError) | 1 | error category matches (Syntax) |
| r3_* rendering (p/tr/tc/r structural tags, nested tables, colspan/cellbg/vm/hm, fix_add/remove, docpr, combo) | 22 | 22 MATCH |
| p4_rt_* / p4_rtp_* rendering (full RichText properties/hyperlinks, RichTextParagraph, rich text in table cells) | 5 | 5 MATCH |
| p4_listing_* rendering (Listing control characters mixed with rich text) | 2 | 2 MATCH |
| p4_img_* rendering (png/jpg/bmp/gif/tiff, scaling, sha1 dedup, multiple images, anchors, row loops, format extensions) | 8 | 8 MATCH |
| p4_combo_rich (RichText + Listing + image + row-loop combination) | 1 | 1 MATCH |
| p4_autoescape_rich (autoescape=True + RichText/Listing/InlineImage safe-value) | 1 | 1 MATCH |
| p4_img_bad (UnrecognizedImageError) | 1 | error category matches (Image) |
| p5_hf_basic / p5_hf_multi / p5_hf_untagged (header/footer variables/if/paragraph loops, multiple sections, untagged round-trip) | 3 | 3 MATCH |
| p5_hf_richtext (header RichText/Listing + body RichText external links) | 1 | 1 MATCH |
| p5_hf_image (2 header images including an anchor, 1 footer image, 1 body image; multi-owner rels/sha1 sharing) | 1 | 1 MATCH |
| p5_footnotes_basic (footnote variables/RichText/Listing + CT sort normalization) | 1 | 1 MATCH |
| p5_hf_syntax_error (header TemplateSyntaxError) | 1 | error category matches (Syntax, part=word/header1.xml) |
| p6_subdoc_basic / p6_subdoc_verbatim / p6_subdoc_untagged (subdoc ordinary-paragraph fragments, literal jinja tags not evaluated twice, empty-body fragments) | 3 | 3 MATCH |
| p6_subdoc_style (style name mapping reuse + custom-style deepcopy append + numbering/linked chains) | 1 | 1 MATCH |
| p6_subdoc_image (subdoc image byte-sha1 merge, media/main rels/CT finalized + external hyperlink relationship migration) | 1 | 1 MATCH |
| p7_media_body / p7_media_header (CRC32 media replacement; body/header references to the same media part hit globally) | 2 | 2 MATCH |
| p7_pic_match (cNvPr name/title identify two replaced images; registration insertion order matched) | 1 | 1 MATCH |
| p7_pic_missing (replace_pic identifier miss ValueError) | 1 | error category matches (Value) |
| p7_embedded_zipname (embeddings CRC replacement + exact zipname replacement of the OLE part) | 1 | 1 MATCH |
| p7_replace_only (skip_render save without rendering, docPr ids keep original values + CRC media replacement) | 1 | 1 MATCH |
| p7_undeclared_vars (body+story undeclared-variable introspection, loop variables excluded automatically) | 1 | 1 MATCH |
| p7b_* real-Word templates (16-item upstream corpus from docxtpl 0.20.2: 7 of if/for/nested tables/filters/run splitting/whitespace control/space preservation/vm/hm/literal; 2 entity and tree round-trips; 4 RichText python contexts; customXml package-level normalization/B1 throughout; redundant-local-xmlns merging vm_nested; footnotes original-byte round-trip footnotes_real) | 16 | 16 MATCH |
| rt_* real-docx OPC round-trip (docxtpl-opc fixture_roundtrip) | 20 | 20 per-part byte matches |
| patch_xml golden (full_patched, docxtpl-compat golden_patch) | 102 | 102 byte matches |
| stages recover golden (equal tree structure, docxtpl-xml golden_recovery) | 100 | 100 matches |
| **Total** | **324 assertions/cases** | **all pass within the frozen denominator; unsupported, DEV, and post-freeze dynamic regressions are not counted in that denominator** |

Conclusion: within the 0.8.0 frozen denominator (the §4
P2/P3/P4/P5/P6/P7/P7b matrices + render_properties), output is **byte-for-byte
equivalent** to the Python oracle (DEV-0003 entry-order/timestamp normalization
was not triggered: the actual output already matches even on raw part bytes —
including the media parts newly added in P4, the rebuilt document rels and
[Content_Types].xml, the P5 multi-owner story rels and CT sort normalization,
the style/image parts, main rels, and CT changes written by P6 subdoc merging,
the in-place replaced media/embeddings blobs and the save-without-rendering
path in P7, as well as the save-time CT/rels/styles/settings/numbering
normalization, cross-tree namespace merging, and footnotes original-byte
round-trip of the P7b real-Word templates, ADR-009).
The stable categories of error cases
(`TemplateErrorKind::Syntax` / `TemplateErrorKind::Image` /
`TemplateErrorKind::InvalidArgument`) align with the upstream exception
classification, and story/subdoc/replacement errors carry concrete part names.

The counts above are historical acceptance results for the frozen
manifest/golden; compatibility-hardening regressions added later are not
counted in the 324, and dynamically constructed high-risk Subdoc samples are
not described as per-part byte-equivalent.

Currently known boundaries (not part of the original frozen acceptance):
DEV-0006 footnote images; DEV-0007 same-part dual rels/external stories and
the forward endnote renderer; the Subdoc borrowing mode and JSON values of DEV-0008; the
deterministic `w:nsid` of DEV-0010 does not replicate upstream random bytes;
DEV-0011 intentionally fixes the upstream dangling-footnote-relationship
defect; Python `jinja_env` / extension object injection in DEV-0013; the
low-level fragment of DEV-0014 does not migrate relationships/styles/media; the
i128 bigint cap of DEV-0016; InlineImage transform-type filters of DEV-0017;
the remaining VM/type-model boundaries of DEV-0018; and `urlize`/template-loader
gaps in DEV-0019. The libxml2 recover long tail of
DEV-0002 is pinned only by corpus and probe rules; byte equivalence is not
promised for inputs outside the corpus. Custom properties, numbering-part
creation/restart, SmartArt, VML, footnote merging, and multi-section documents
have been covered by later semantic probes and integration regressions;
DEV-0005, DEV-0009, and DEV-0012 are closed.
