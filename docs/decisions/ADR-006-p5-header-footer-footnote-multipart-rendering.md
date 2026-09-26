# ADR-006: P5 Header/Footer/Footnote Multi-Part Rendering

Date: 2026-09-25
Status: Accepted
Phase: P5 (headers, footers, footnotes)

## Context

Rendering in P0–P4 only rewrites `word/document.xml`, but in real documents variable content also appears in
header/footer story parts (`word/headerN.xml`, `word/footerN.xml`), core properties
(`docProps/core.xml`, supported since P3), and the footnotes part (`word/footnotes.xml`). Upstream
`DocxTemplate.render` has distinct processing paths for these parts, and the relationship/media side effects of
InlineImage must be isolated by scope across multiple parts. This ADR records the P5 part orchestration, serialization
differences, and multi-owner model.

## Upstream Facts Pinned Down by Probes (docxtpl 0.20.2 + python-docx 1.2.0 + lxml 6.1.1)

1. **Fixed rendering order** (the render body of `template.py`): body
   (`fix_tables` + `fix_docpr_ids`) → headers (in the order of header relationships in the main-document
   rels) → footers (in footer-relationship order) → `render_properties` → `render_footnotes`.
2. **header/footer are rendered via `get_part_xml` + `render_xml_part`**:
   - `get_part_xml` first does `parse_xml(part.blob)` (the python-docx oxml parser,
     `remove_blank_text=True`) and then `etree.tostring(unicode)`; afterwards it shares with the body the
     `patch_xml`, `\n<w:p` line insertion, jinja, restoration, and `resolve_listing` steps; it does **not**
     run `fix_tables` / `fix_docpr_ids`.
   - On save, `map_headers_footers_xml` uses `XmlPart.load` to **rebuild the part** from the rendered string
     (a newly parsed, independent tree, not re-grafted onto any original element), ending with
     `etree.tostring(el, encoding="UTF-8", standalone=True)` → the single-quote declaration + newline; the new
     part copies all rels of the original part.
3. **Two serialization differences for story parts** (substantiated by oracle diffs):
   - `remove_blank_text` strips pure-whitespace text between elements (the newlines/indentation that the
     python-docx template itself puts inside injected image XML disappear, and the image fragment becomes compact),
     but whitespace text within the scope of `xml:space="preserve"` is retained (libxml2 tracks this attribute along
     the ancestor axis: `preserve` keeps it, `default` restores stripping).
   - The injected `wp:inline` carries the four xmlns declarations `wp/a/pic/r`; for the body, `map_tree`
     **re-grafts** the rendered tree under the original document element, so lxml serialization strips the
     `xmlns:wp/xmlns:r` declarations duplicated with ancestors (only a/pic remain, and whitespace is kept as
     multi-line indentation); stories have no re-grafting, so the four redundant declarations are retained as-is.
4. **Footnotes are generic binary Parts**: the footnotes content type is not registered in the python-docx
   PartFactory, so the path is `part.blob.decode()` → patch → `render_xml_part`
   → `part._blob = xml.encode()`; the template's XML declaration and unchanged bytes are preserved as-is, with no
   XML re-parse/re-serialization. Resolving an InlineImage on a generic Part triggers
   `AttributeError` (upstream does not support footnote images; registered as DEV-0006).
5. **shape_id is a part-level constant (correcting the earlier statement in ADR-005)**:
   in python-docx 1.2.0, `StoryPart.next_id` is an uncached `@property`; every call takes
   `xpath("//@id")` on the **original part tree** and returns max+1; rendering only produces strings and does not
   write back to part elements, so the docPr id/name of every new image in the same part are identical (for the body,
   `fix_docpr_ids` then rearranges ids from 1001, leaving names untouched; stories and footnotes are not renumbered).
6. **Image relationships are allocated in the scope of the currently rendering part**: `current_rendering_part`
   switches to the corresponding XmlPart, and the InlineImage blip/hyperlink relationships and newly created rels all
   land in that part (a rels part is created if absent); media parts and `media/imageN.ext` numbering remain
   package-level, shared via sha1 deduplication. External links from `build_url_id` always belong to the main-document
   rels.
7. **[Content_Types].xml is rebuilt on every save** (`PackageWriter`):
   it is reassembled from all parts, with Default sorted by extension and Override by part name in ASCII
   order; even if this render added no images, out-of-order Overrides (such as a tool-inserted footnotes Override) are
   normalized on save.

## Decision

1. **Three rendering-pipeline modes** (docxtpl-template):
   - `render_document_xml_ctx` (Document): the full pipeline + `fix_tables` +
     `fix_docpr_ids`, with normal serialization (redundant-ns stripping, whitespace text retained).
   - `render_story_xml_ctx` (Story): the full patch/jinja/listing pipeline but without fixes; before serialization
     `strip_blank_text` (respecting `xml:space`), with output going through the new
     `serialize_story` (which retains redundant xmlns declarations carried lexically by elements).
   - `render_footnotes_xml_ctx` (Footnotes): only the string stage runs and the result is returned as-is
     (preserving the template declaration), internally using `NullRegistry`; an InlineImage is an error.
2. **Part orchestration is centralized in the facade `render_all_parts`** (docxtpl-rs):
   body → story parts → core properties → footnotes, in the same order as upstream; story targets are enumerated via
   the main-document rels (two passes: first `/header`, then `/footer`), collecting only non-empty parts that are
   Internal, whose relative Target resolves (via `PartUri::parent()` — the **directory containing** the main-document
   part, not the part path itself) to a hit, deduplicated by target; footnotes are enumerated by filtering package
   parts on content type.
3. **Multi-owner image registry** (`ImageInjections`): owner state
   (name/rels_name/rels/rels_existed/dirty) is pushed and registered, `begin_owner` switches idempotently; a new
   rels part is first mounted with `add_part` and then populated with `set_part_bytes`; `apply` finalizes pending
   media and all dirty owner rels uniformly. sha1/media numbering stays package-level and shared; `build_url_id`
   forces current = main document.
4. **Lazy image placeholder resolution**: `context_to_minijinja` encodes InlineImage as control-character
   placeholders; after jinja rendering, a regex resolves them in order of appearance in the output (unreferenced images
   produce no relationships; repeated appearances of the same image are each resolved once, with rId reuse and docPr
   sharing the same constant shape_id); if any single image fails to resolve, the whole part returns an error carrying
   the part name.
5. **Two new docxtpl-xml capabilities**: `XmlDocument::strip_blank_text`
   (deletes pure-whitespace text nodes according to xml:space scope) and
   `serialize_story` (serialization with a `retain_redundant_ns` option); used only in Story mode, leaving byte
   behavior of the body/footnotes paths unchanged.
6. **Normalize [Content_Types].xml after every render**: `canonicalize_content_types` rebuilds via the existing
   table's `to_xml()` (Default/Override sorting) and does not write back when the bytes are unchanged (dirty gating).
7. **Fixtures**: add 7 P5 fixtures (hf_basic, hf_multi,
   footnotes_basic, hf_image, hf_richtext, hf_syntax_error (error expected),
   hf_untagged); oracle diffs for python contexts (footnote rich values, multiple images including anchors,
   RichText+build_url_id) are replicated 1:1 inside the tests. Fixtures/goldens must not be altered to relax
   assertions.

## Impact

- No new user-facing public API (multi-part rendering happens automatically);
  `render`/`render_ctx`/`RenderSession::finish` behavior is unified.
- Error messages now carry the concrete story/footnote part name (e.g.
  `part: "word/header1.xml"`).
- Known limitations (compatibility.md §5): DEV-0006 — footnotes do not support InlineImage;
  DEV-0007 — duplicate header/footer relationships to the same part render only once, and endnotes are out of
  scope.
- Acceptance: all 73 render fixtures are byte/c14n-identical to the Python oracle per part
  (70 successful matches + 3 error-category matches), with golden counts of
  73 (patch) / 71 (recovery).

## Post-Freeze Addendum (2026-09-26)

- Stories only exactly match the full official header/footer relationship URIs, and only scan Internal, existing,
  non-empty targets of the main-document rels; External, orphan stories, and endnotes are not rendered.
- When the same target is referenced by multiple relationships it is still deduplicated by target; footnotes are now
  rendered in a single pass per part, avoiding multi-section documents treating the first output as a template again.
  The complete boundary is covered by DEV-0007.
- XmlParts such as the body, header/footer, and core accept UTF-8 BOM, UTF-16LE/BE, and UTF-32LE/BE based on the
  initial XML byte pattern, and are uniformly saved as UTF-8; generic footnotes still follow upstream
  `blob.decode()` and accept only UTF-8. The current encoding evidence comes from Rust package-level regressions and is
  not counted in the historical 73-item denominator.
- The InlineImage placeholder subsequently switched to a randomized namespace per context conversion; for its security
  wording and transform-filter boundaries see ADR-005 / DEV-0017.
