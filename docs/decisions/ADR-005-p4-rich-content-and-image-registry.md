# ADR-005: Typed Context for P4 Rich-Content Values and the Image Registry

Date: 2026-09-25
Status: Accepted
Phase: P4 (RichText and images)

## Context

The P2/P3 rendering context is pure JSON (`serde_json::Value`), because upstream `tpl.render(context)`
only performs textual substitution for variables inside templates. P4 introduces three kinds of upstream "typed
values": `RichText`/`RichTextParagraph` (which construct `w:r`/`w:p` XML fragments), `Listing`
(escaped text, reusing the existing resolve_listing), and `InlineImage` (which reads image bytes at render time,
writes a media part, allocates an rId, and injects `wp:inline`). They cannot be expressed in JSON, and
`InlineImage` has cross-part package-level side effects.

## Upstream Facts Pinned Down by Probes (docxtpl 0.20.2 + python-docx 1.2.0 + lxml 6.1.1)

1. **`InlineImage.__str__` output is python-docx `BaseOxmlElement.xml`**
   (`etree.tostring(..., pretty_print=True)`): the `wp:inline` tree has no whitespace, and on output is
   pretty-indented by 2 spaces per level, with a newline after the root closing tag; the outer wrapper is the
   split-run string
   `</w:t></w:r><w:r><w:drawing>…</w:drawing></w:r><w:r><w:t xml:space="preserve">`.
2. **Host serialization strips redundant namespace declarations**: the string-injected `wp:inline` carries
   the four `wp/a/pic/r` declarations; after recovery parsing + lxml serialization, declarations that duplicate a
   "same prefix, same URI" binding on the ancestor axis are stripped (probes A/B/C/D confirm: different URIs are kept,
   siblings do not participate, and default prefixes are handled the same way), while the remaining declarations keep
   their original relative order — ultimately only `xmlns:a` and `xmlns:pic` remain.
3. **shape_id semantics**: `StoryPart.next_id` takes `//@id` (numeric values) on the **original document
   tree before rendering starts** and returns max+1; the tree is not updated during the string-rendering phase, so the
   `wp:docPr` id/name of multiple images in the same document are identical (e.g. all `id="1" name="Picture 1"`);
   uniqueness is repaired after rendering by `fix_docpr_ids` (renumbering from 1001). `pic:cNvPr` is always
   `id="0"`, and name is the image file's basename.
4. **rId allocation**: `Relationships._next_rId` backfills the first hole starting from rId1 (not
   max+1); image relationships are created on demand (`relate_to`, reused when the same reltype+target is hit), and
   hyperlink (anchor) external relationships are appended at the tail. Image parts are deduplicated by sha1, and
   `word/media/imageN.ext` numbering likewise takes the first hole.
5. **Dimensions**: `EMU = int((px/dpi)*914400)` (f64 divide → multiply → truncate); when only width or only height
   is given, banker's rounding preserves the aspect ratio (`round`); when both are given they are used as-is with no
   constraint applied.
6. **rels and [Content_Types].xml are fully rebuilt when python-docx saves**: rels are
   `<Relationships xmlns=…>` (attribute order Id/Type/Target/[TargetMode]), and Content Types consist of
   Default entries (sorted by extension) + Override entries (sorted by partname), both with the lxml single-quote
   declaration + newline. During P0–P3 these two part types had unchanged content, so the original bytes happened to
   match the rebuilt result; once P4 adds relationships/images, they must be rebuilt in this format.

## Decision

1. **Add a new `docxtpl-rich` crate** (corresponding to the P4–P6 rows of the component table in project doc
   §5):
   - `RichText`/`RichTextParagraph`/`Listing`/`InlineImage` types, with `to_xml()` byte-aligned with
     upstream (including the five-character `& < > " '` escaping of `html.escape(..., quote=True)`).
   - `Image`: png/jpeg/gif/bmp/tiff header parsing (sha1, px, dpi, ext, content type), replicated field by field from
     the probes and the python-docx source.
   - `RenderValue`: `Json(..)` | `RichText(..)` | `RichTextParagraph(..)` |
     `Listing(..)` | `InlineImage(..)`; `RenderContext` is an ordered map.
   - minijinja bridge: rich values are wrapped as `ObjectRepr::Plain` custom objects implementing
     `Object::render`, equivalent to upstream `__str__` (`{{ v }}` expands the XML directly).
     With autoescape=True upstream still does not escape via `__html__`; whether this side's render output is escaped
     by minijinja is outside the oracle scope (all fixtures use autoescape=False), and this difference is registered
     as a known P4 limitation (see compatibility.md §5).
2. **Generalize the rendering pipeline** (docxtpl-template): `render_document_xml` gains a
   `RenderContext` variant and an `ImageRegistry` parameter; `shape_id` is computed before rendering from the
   original `word/document.xml` (once, shared by all InlineImages); all package-level services needed by
   `render_xml`/`InlineImage` are delivered through
   `ImageRegistry::resolve(..) -> {blip_rid, hyperlink_rid, filename, cx, cy,
   shape_id}`, and the template crate does not depend on OPC.
3. **The facade implements `ImageRegistry`** (docxtpl-rs), operating on `Package` —
   sha1 deduplication, media part naming, rId hole allocation, and python-docx-style rebuilding of
   `[Content_Types].xml` and document rels; after rendering completes, the media part bytes are written (as-is) into
   the package.
4. **The docxtpl-xml serializer gains lxml stripping semantics**: when outputting an element, skip its own nsdecls
   that are "same prefix, same URI on the ancestor axis", and keep the rest in original order (probe A–D rules). This
   behavior has no impact on existing goldens (their subtrees contain no redundant declarations); the 48 recovery
   goldens must all pass in regression.
5. **Fixture evolution**: `generate.py` produces `p4_*` templates and image samples (home-made
   2×1 PNG, 4×2 300dpi JPEG, etc. with reproducible bytes); `manifest.json` gains
   `context_kind` (`json`|`python`); `runner.py` loads `build_context(tpl)` for python contexts;
   `dump_stages.py` emits p4 stages goldens in lockstep. Existing fixtures/goldens must not be modified.

## Impact

- Public API: add `DocxTemplate::render_ctx(&RenderContext, &RenderOptions)`; the existing
  `render(&JsonValue, ..)` is unchanged (internally converted to RenderContext).
- CLI: image-injection arguments such as `docxtpl render --image <name>=<path>[:WxH]` are deferred to a separate
  review; P4 is API-focused (there is no upstream CLI behavior, so no behavior should be invented).
- Compatibility: the P4 diff criteria are unchanged (byte-exact part + c14n); all new p4_* fixtures are incorporated
  into compatibility.md §7.

## Post-Freeze Addendum (2026-09-26)

- The rich-value wrapper subsequently gained Python-object always-truthy semantics, `is string == false`, and the
  `__html__` safe identity; with autoescape=True, direct output and `escape` inject as-is, while ordinary strings
  produced by `string` and similar filters continue to be escaped. This path is pinned down by the
  `p4_autoescape_rich` and template-crate regressions, and the former DEV-0005 is closed.
- `RichTextParagraph::with_text("")` produces no paragraph, whereas constructing one and then explicitly calling
  `add("")` still produces an empty paragraph; an InlineImage with an empty anchor creates no hyperlink relationship.
- The image lazy placeholder uses an independent randomized namespace for each context conversion, avoiding accidental
  collisions with ordinary input or the old fixed token; this is not offered as a cryptographic unpredictability or
  unforgeability guarantee. For the remaining object semantics of images passed through transform filters, see
  DEV-0017.
- A forward-compatible API may write `title`/`descr` into both `wp:docPr` and `pic:cNvPr`, and retain explicit
  empty-string attributes. This capability comes from docxtpl master and is not counted in the 0.20.2 denominator.
- filename/title/descr are pre-checked for XML 1.0 characters before the image lands, and the 64 MiB budget is enforced
  on the aggregate size of the two post-attribute-escaping landing points. The public `ImageError` consequently gains
  `InvalidXmlMetadata` and `MetadataTooLarge { max }`; exhaustive matchers must handle the new variants.
