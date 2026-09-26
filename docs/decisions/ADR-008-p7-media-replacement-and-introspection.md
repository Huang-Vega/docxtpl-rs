# ADR-008: P7 Media/Embedded Replacement Family and Template Introspection (replace_media / replace_pic / replace_embedded / replace_zipname / reset_replacements / get_undeclared_template_variables)

Date: 2026-09-26
Status: Accepted
Phase: P7a (media replacement family + undeclared-variable introspection)

## Context

P4–P6 solve "inject new content at render time". Upstream docxtpl also has a family of APIs that **replace existing
binary parts** at save time (`template.py` L749-878), plus one helper that does no rendering and only performs static
template introspection (L894-927):

- `replace_media(src_file, dst_file)`: registers by CRC32 of the source file bytes, and after saving replaces the
  content of entries under `word/media/` in the package whose CRC matches with the target bytes;
- `replace_embedded(src_file, dst_file)`: the same, acting on `word/embeddings/`;
- `replace_zipname(zipname, dst_file)`: replaces any part exactly by full zip entry name;
- `replace_pic(embedded_file, dst_file)`: in the pre-processing stage, locates DrawingML pictures by their cNvPr
  name/title/descr identifiers and replaces the target part blob in place;
- `reset_replacements()`: clears the four kinds of registrations above;
- `get_undeclared_template_variables(context=None, jinja_env=None)`: runs patch_xml over the body and all
  headers/footers and then performs undeclared-variable meta-analysis with a bare jinja environment.

This ADR records the P7a porting decisions. Larger piggyback features such as upstream `docx` (rich-text image
insertion) are not in P7a.

## Upstream Facts Pinned Down by Probes (docxtpl 0.20.2 + python-docx 1.2.0)

1. **Byte replacement happens when the zip is reopened after save** (post_processing,
   template.py L749-786): decided per entry, with priority
   `filename in zipname_to_replace` (exact full name, leading `/` stripped first) >
   `startswith("word/media/") and CRC in crc_to_new_media` >
   `startswith("word/embeddings/") and CRC in crc_to_new_embedded`;
   CRC = `binascii.crc32(buf) & 0xFFFFFFFF` (same algorithm and result as
   `crc32fast::hash`). Only the blob changes: part name, `[Content_Types].xml`, rels, and `wp:extent`
   dimensions all stay untouched. Images referenced by headers are also hit because media parts are globally unique.
2. **replace_pic runs in pre_processing** (_replace_pics, L793-878):
   scans the main document part, then scans each target part in the **order of appearance** of HEADER/FOOTER
   relationships in the main rels (no deduplication, headers first then footers). Per part it takes
   `//a:graphic/a:graphicData`, handling only the picture form with `@uri == nsmap["pic"]`; it takes the
   relationship id from `pic:blipFill/a:blip/@r:embed` (continue if absent), `@name` from
   `pic:nvPicPr/pic:cNvPr` (when missing, xpath[0] raises IndexError, which is swallowed wholesale by the
   per-graphicData `except Exception`), with `@title`/`@descr` defaulting to the empty string.
3. **Identifier matching follows dict registration insertion order and breaks on the first hit**: each
   graphicData tries every registered img_id in turn, and equality with any of name/title/descr replaces
   `doc_part.rels[rId].target_part._blob` (when the same part is referenced from multiple places there is only one
   blob, and all references take effect simultaneously). Earlier-registered keys shadow later-registered keys on the
   same picture (substantiated by p7_pic_match: both images have the default python-docx fixed base name
   `"image.png"`, so the second one must be renamed for a title key to possibly hit independently).
4. **allow_missing_pics defaults to False**: after all parts are scanned, any registered identifier that did not hit
   raises `ValueError("Picture %s not found in the docx template")`.
5. **Saving directly without rendering** (the L887-888 path): a fresh
   `Document(template_file)`, without fix_tables/fix_docpr_ids, so body docPr ids keep the template's original values
   (the rendering path would rearrange them from 1001); pre/post replacement handling still runs as usual.
6. **get_undeclared_template_variables**: a new Document,
   `patch_xml(xml_to_string(body))`, then two passes over the main rels
   (HEADER_URI, FOOTER_URI) appending `patch_xml(xml_to_string(parse_xml(blob)))` for targets whose blob is non-empty;
   bare `Environment().parse` + `meta.find_undeclared_variables`. It is substantiated that the conjunctive result of
   `{{a}}{%if b%}{{c}}{%endif%}` and `{%p for item in items%}{{item.name}}{%p endfor%}` is
   `{a,b,c,items}` — the loop variable item is automatically excluded by jinja meta-analysis while `items` is
   retained. The optional `context` set-difference parameter and custom `jinja_env` are not ported (DEV-0013).
7. **Embeddings mounting**: after `Part(PackURI("/word/embeddings/x.bin"),
   content_type, blob, package)` + `doc.part.relate_to(part,
   RT.oleObject)`, the python-docx save retains the part and CT Override, with no need to manually modify
   `[Content_Types].xml`.

## Decision

1. **Error classification**: `TemplateErrorKind::InvalidArgument` →
   `oracle_exception()` maps to `"ValueError"` (aligned with a replace_pic missing identifier; upstream's other
   replacement APIs do no existence checks, and CRC/zipname misses are silent no-ops).
2. **New module `docxtpl-rs/src/replacements.rs`**: `Replacements` holds four registration tables — media/embedded are
   `HashMap<u32, Vec<u8>>` (key = CRC32 of the source bytes), zipnames are `HashMap<String, Vec<u8>>`, and pics are an
   **order-preserving `Vec<(String, PicReplacement)>`** (Decision 3). `reset()` clears all four categories, aligned with
   reset_replacements.
3. **pics is an order-preserving Vec rather than a map**: upstream's `pics_to_replace` is a dict where matching follows
   insertion order and breaks on the first hit, and missing errors also follow insertion order; duplicate registration of
   the same identifier follows dict assignment semantics, overwriting the bytes while keeping the first registration
   position.
4. **Two landing points**:
   - `apply_pic_replacements` (pre path, executed both with and without rendering):
     reuses docxtpl-xml's `parse_strict`/xpath-equivalent manual traversal (`descendants` includes the element itself so
     the root must be skipped, and `""` is passed when there is no namespace attribute); any structural gap per
     graphicData skips it wholesale (aligned with upstream swallowing exceptions); header/footer targets are enumerated
     in order of appearance in the main rels without deduplication (an in-module standalone
     `header_footer_targets`, distinct from the deduplicated `story_parts` used for rendering in lib.rs). A non-UTF-8 blob
     returns a `NotUtf8` error carrying the part name (document/header/footer of a well-formed docx must be UTF-8 XML, so
     this is normally unreachable).
   - `apply_byte_replacements` (post path): zipname exact (leading `/` stripped) > `word/media/`+CRC >
     `word/embeddings/`+CRC; only part bytes change, not the part name/CT/rels.
5. **Session orchestration**: `RenderSession` gains a `replacements` field and five chained methods
   (`replace_media`/`replace_embedded`/`replace_zipname`
   /`replace_pic`/`reset_replacements`, each taking `impl AsRef<[u8]>` and `&str` and returning `&mut Self`); at the end
   of `finish()`, pic replacement → CT normalization → byte replacement → OPC validation run in that order; a new
   `finish_without_render()` aligns with saving directly without rendering (no fix_tables/fix_docpr_ids, docPr ids keep
   the template's original values).
6. **Template introspection** (docxtpl-template):
   `render::find_undeclared_variables(doc_xml, story_xmls)` patches the body subtree and each story root element
   separately, concatenates them, collects with minijinja `template.undeclared_variables(false)`, and returns
   `BTreeSet<String>` (stable ordering, equivalent to Python sorted).
   `DocxTemplate::undeclared_variables()` reuses the rendering-side story_parts enumeration (the deduplicated version).
   The introspection parse-tree root node is the top-level element itself (no virtual Document layer); strict-parse
   failure is reported as an XML error.
7. **Fixtures**: add 7 `p7_*` fixtures (media_body/media_header/
   pic_match/pic_missing/embedded_zipname/replace_only/
   undeclared_vars), all with context_kind="python";
   `p7_replace_only` is marked `skip_render: true`, and the runner correspondingly skips
   `tpl.render(context)` but still runs `tpl.save`; 8 fixed-byte material files go into
   `tests/fixtures/media/` (the PNG is deterministically hand-constructed, the embed is fixed ASCII). Fixtures/goldens
   must not be altered to relax assertions.

## Impact

- New public API: `RenderSession::replace_media` /
  `replace_embedded` / `replace_zipname` / `replace_pic` /
  `reset_replacements` / `finish_without_render`;
  `DocxTemplate::undeclared_variables`;
  `docxtpl_template::find_undeclared_variables`.
- New dependency: `crc32fast = "1.5"` (the same IEEE polynomial as Python binascii.crc32, pinned by per-fixture diffs).
- Acceptance: all 85 render fixtures match the Python oracle (81 byte-exact per-part
  matches + 4 error categories: Syntax×2, Image×1, Value×1), and all 7 p7 fixtures MATCH
  (including the skip_render no-render path); golden counts are
  85 (full_patched) / 83 (pre_recover).
- Known limitation: DEV-0013 (compatibility.md §5).

## Post-Freeze Addendum (2026-09-26)

- `undeclared_variables_with_context/with_keys` and `allow_missing_pics(true)` are post-freeze compatibility hardening;
  they do not retroactively rewrite the 85-item historical acceptance denominator of this ADR.
- The Rust-native environment configurator is used only for actual rendering and does not enter static template
  introspection; Python `jinja_env` / extension objects are still rejected (DEV-0013).
- The skip-render picture check and the known XmlPart save path can decode UTF-8/16/32; the current direct evidence is
  Rust package-level regressions rather than the original P7 Python fixtures.
