# ADR-007: P6 Subdocument Merging (Subdoc / new_subdoc)

Date: 2026-09-25
Revised: 2026-09-26 (compatibility hardening)
Status: Accepted
Phase: P6 (Subdoc subdocument merging)

## Context

P0–P5 cover all variable parts of a single template itself. Upstream docxtpl additionally provides
`tpl.new_subdoc(docpath)`: **while constructing the context**, it merges the parts of an external docx (styles,
numbering, relationships, media, Content Types) into the main document package, while the body content is injected as
an XML fragment value through the template slot `{{p sd }}`. This capability is implemented by
docxtpl 0.20.2's `Subdoc` calling docxcompose 2.2.0's `Composer` (`attach_parts`). This ADR records the P6
merge orchestration, persistence strategy, subsequent hardening of high-risk paths, and the deterministic/security
deviations that remain.

## Upstream Facts Pinned Down by Probes (docxtpl 0.20.2 + docxcompose 2.2.0 + python-docx 1.2.0 + lxml 6.1.1)

1. **The template slot is the expression `{{p sd }}`, not a statement tag**: patch_xml step G promotes the
   carrying whole `w:p` to a bare `{{ sd }}`, and the rendering path is fully isomorphic to
   RichTextParagraph (P4); `Subdoc.__html__`/`__str__` = `_get_xml()` = `etree.tostring(body)`
   followed by regex stripping of the `<w:body>` opening/closing tags. xmlns declarations promoted onto the body tag
   are lost together with the tag, and the fragment relies on the main document's root declarations as fallback.
2. **Single-pass jinja evaluation**: `{% x %}`/`{{ y }}` text inside the fragment appears verbatim in the
   rendered result and is not evaluated a second time (pinned by p6_subdoc_verbatim).
3. **Construction-time merging**: when `Subdoc(tpl, docpath)` is constructed, `attach_parts` runs immediately,
   and all part changes are persistently written into the main package before rendering; the body element has **no
   deepcopy** (the sub's own tree is modified directly), and there is no fix_header_and_footers.
4. **Fixed attach_parts order** (docxtpl/subdoc.py):
   custom properties `dissolve_fields` → build style id/name maps →
   per direct child of each sub body (skipping `w:sectPr`): add_referenced_parts →
   add_styles → add_numberings → restart_first_numbering →
   add_images → add_diagrams → add_shapes → add_footnotes →
   remove_header_and_footer_references; after the loop
   add_styles_from_other_parts → renumber_bookmarks →
   renumber_docpr_ids → renumber_nvpicpr_ids → fix_section_types.
5. **add_referenced_parts**: `.//*[@r:id]` in document order; IMAGE/
   HEADER/FOOTER reltypes are skipped (left dangling and handled by add_images and
   remove_header_and_footer_references respectively); external →
   get_or_add on the main rels (idempotent by reltype+target+mode); internal →
   `copy_part` recursively copies the whole part/rels graph, renumbering partnames by taking the prefix from
   `FILENAME_IDX_RE = ([a-zA-Z/_-]+)([1-9][0-9]*)?` and backfilling holes; source rels are copied in rId
   numeric order (when copy rIds are contiguous they match the source).
6. **Three style branches**: used_style_ids are deduplicated in order via OrderedDict.fromkeys
   (w:val of tblStyle/pStyle/rStyle); a mapping from sub style id → w:name →
   main style id. If the main style element is identical → keep; if absent in main → append a deepcopy copy along with
   its numbering and linked styles; if the main has a different id → rewrite references by fall-through along the
   numbering↔linked chain (a missing link at any hop is a no-op; you **must not early continue**, otherwise tail
   reference rewrites are missed).
7. **Numbering merge**: `_next_numbering_ids` is taken once before the loop — numId max+1 of w:num (1 if none),
   abstractNumId max+1 of w:abstractNum (0 if none); `_insert_num` actually inserts **before the last w:num**
   (the upstream source comment saying "after" contradicts the code), and `_insert_abstract_num` inserts before the
   first num (insert(0) if there is no num); if the sub lacks the corresponding
   abstractNum, continue (the mapping residue, skipped num insertion, and the trailing dangling
   numId rewrite still run); when abstractNum contains `w:nsid`, upstream rewrites nsid with
   `random.random()` — **non-deterministic output**.
8. **restart_first_numbering is always called with restart=True**
   (docxcompose 2.2.0; the "False" written in earlier planning documents was an error). Any hit in the guard chain
   causes a normal exit: heading-style outlineLvl, bullet numFmt, no
   pStyle, or no numId on the style/body; the numbering restart executes only when the modification block is actually
   entered. When the main package lacks word/numbering.xml, upstream creates that part from the built-in default
   template.
9. **add_images**: `(.//a:blip|.//asvg:svgBlip)[@r:embed]` in document order; the ImageWrapper extension is taken
   from the **source part filename suffix** and the content type from the **source package [Content_Types].xml
   declaration** (not byte-header detection; the "byte detection" written in earlier planning documents was an error),
   with sha1 over bytes; on a hit the partname/rId is reused, and on a miss `word/media/imageN.ext` backfills holes
   across extensions; `r:link` external goes through add_relationship.
10. **The three renumber siblings act on the main document**: bookmarkStart and bookmarkEnd
    each count independently from 0; wp:docPr and pic:cNvPr each from 1, continuing after the body on the HEADER/FOOTER
    targets in the main rels (in insertion order, without deduplication) with the same counters; docPr inside the body is
    subsequently rearranged from 1001 by the rendering-time fix_docpr_ids, while numbering inside hf parts stays in
    effect (P5 story rendering does not run fix_docpr_ids).
11. **fix_section_types**: if either side has ≤1 sections (counting body-direct sectPr plus
    sectPr inside pPr), it is a no-op; when both sides are multi-section, the main section-start type must be rewritten.
12. **Full python-docx save**: docxtpl save traverses the graph via iter_parts and rebuilds CT/rels with
    PackageWriter, so all tree changes made during compose land on disk.
    `_ContentTypesItem._add_part` (register_content_type): if a Default with the same extension already exists and the CT
    matches → leave it; same extension with a different CT → add_override;
    no Default for the extension → add_default.
13. **Custom properties are used only to dissolve fields**: `CustomProperties` locates `docProps/custom.xml`
    from the custom-properties relationship of the package-root `_rels/.rels`; for each property name it dissolves the
    simple/complex `DOCPROPERTY` fields in the subdocument, preserving the cached result content, but does not copy the
    custom part or the root relationship into the main package.
14. **SmartArt and VML belong to two dedicated paths**: the `r:dm`/
    `r:lo`/`r:qs`/`r:cs` of `dgm:relIds` are forced to the diagramData/layout/quickStyle/
    colors relationship types respectively and copy the referenced graph; the same source URI/target relationship type
    should reuse an already copied part. `v:shape/v:imagedata` shares media sha1 deduplication and numbering with
    DrawingML images, and rewrites `r:id`.
15. **Footnote merging**: when the main package has no footnotes, the part, document relationship, and CT are created
    from the docxcompose empty template; when the part exists, it is appended. Referenced footnotes have their ids
    renumbered by the current child-element count of the main footnotes, and intra-footnote relationships are migrated.
    In docxcompose 2.2.0, the relationship-rewrite landing point may leave stale rIds on actually appended clones when
    relationships conflict or are reassigned; this is an upstream defect and is not treated as output to replicate.
16. **Fragment namespaces depend on host context**: after `Subdoc._get_xml()` removes the body shell, the fragment
    may continue to use bindings such as `m`, `w14`, and `wp14` already present on the main `w:body`/ancestors;
    cross-tree copying must also generate stable alternative prefixes on prefix conflicts in the target tree, and must
    not mistake legally inherited prefixes for unbound ones.

## Decision

1. **Value type** (docxtpl-template): add `RenderValue::Subdoc(String)` holding the pre-generated fragment;
   `value_to_minijinja` outputs via `Value::from_safe_string` (aligned with `Subdoc.__html__`; injected as-is
   whether autoescape is on or off; the rich-value autoescape deviation of DEV-0005 does not concern Subdoc); no
   `From<String>` is provided (to avoid conflicting with the existing `From<String>→Json`); the JSON context path is
   unsupported (same as InlineImage).
2. **docxtpl-xml capability additions**: add `serialize_subtree` (subtree serialization output from a designated
   node, without an XML declaration), `insert_child_at` (aligned with lxml `element.insert(index, child)`), and
   `deepcopy_element` (deep-copies a cross-document element and promotes the inherited namespaces it actually uses;
   rebinds to a stable `nsN` on target-prefix conflict). `SubdocFragment` additionally provides a parse entry point
   with a host namespace context, where `new_subdoc` passes the in-scope bindings of the main body; the low-level
   fragment API still does not handle relationship/style/media merging.
3. **New module `docxtpl-rs/src/subdoc.rs`**: `SubdocComposer` orchestrates 1:1 with upstream
   attach_parts; all XML parts are loaded with `parse_strict` +
   `strip_blank_text()` (aligned with the python-docx oxml parser's
   remove_blank_text=True).
4. **Persistence strategy**: the main document/styles/numbering/footnotes and header/footer tree parts are dirty-gated,
   preserving unchanged bytes as-is (the DEV-0004 principle), and changed ones are serialized in python-docx form
   (blank-stripped tree + the lxml single-quote declaration); copied non-image parts, their rels, and Content Types
   changes land in the package immediately (`register_content_type` replicates the three `_add_part` branches); image
   parts and main-document rels are staged via `ImageInjections` (scheme X: during merge, used_numbers/by_sha1/
   known_defaults/owners[0] pending state is synchronized, and `RenderSession::finish` finalizes uniformly, eliminating
   number collisions or duplicate pushes during rendering).
5. **docxtpl-opc**: add `ContentTypes::add_override` (Override is appended in order; ASCII sorting happens only in
   to_xml).
6. **Facade API**: `RenderSession::new_subdoc(path) ->
   Result<RenderValue, Error>`, merging at construction time and callable multiple times in the same session;
   malformed sub packages (missing rels/dangling part/missing CT/rel missing or external,
   num lacking abstractNumId, and other inputs for which upstream raises KeyError/IndexError) uniformly return a
   `Malformed` error carrying the part name.
7. **Compatibility hardening of high-risk paths**:
   - When the main package lacks numbering, create an empty part/document relationship/CT; execute
     `restart_first_numbering` in full; run `fix_section_types` when both sides are multi-section;
   - `w:nsid` does not replicate `random.random()`, instead using a deterministic unique uppercase 8-digit
     hexadecimal value based on the copy context (DEV-0010);
   - Implement custom-property field dissolution, SmartArt, VML, and footnote merging; new rIds of intra-footnote
     relationships are rewritten onto the actually appended clones, fixing the upstream dangling-reference risk
     (DEV-0011);
   - The docpath-less current-document borrowing mode and JSON Subdoc values remain unsupported
     (DEV-0008), and the low-level `SubdocFragment` does not auto-merge parts.
8. **Frozen fixtures**: the original P6 added 5 `p6_*` fixtures (basic/style/image/verbatim/
   untagged, all with context_kind="python"), with the sub docx stored at
   `templates/<id>_sub.docx`; the universe avoids bookmarks/images (on the main-template side)/
   multi-section/w:numId/footnotes/dgm/VML/custom props, so the three renumber siblings and fix_section_types take the
   no-op path on the main-document side; on the oracle side the runner really runs docxtpl + docxcompose 2.2.0.
   Fixtures/goldens must not be altered to relax assertions.
9. **Subsequent semantic regressions**: dynamically constructed missing-numbering/nsid/restart/multi-section/
   namespace and VML/SmartArt/custom-property/footnotes packages are covered respectively by Rust integration tests and
   standalone Python oracle probes asserting parts, relationships, ids, and field structure. These probes are not merged
   into the original manifest, nor is byte-exact per-part equality claimed for the high-risk paths.

## Impact

- New public API: `RenderSession::new_subdoc` (RenderValue::Subdoc is re-exported along with the render_ctx types);
  existing `render`/`render_ctx`/`finish` behavior is unchanged.
- Original P6 acceptance: all 78 render fixtures match the Python oracle (75 byte-exact per-part
  matches + 3 error-category matches), of which the 5 p6 fixtures are all byte MATCH
  (styles.xml keeps its original bytes when only existing styles are reused; for image-bearing fixtures, media/main
  rels/CT are finalized once by finish); golden counts are 78
  (full_patched) / 76 (pre_recover). Subsequent high-risk compatibility items are accepted via semantic probes and
  dynamic integration regressions, without being appended to this frozen count.
- Known limitations: DEV-0008 (no docpath borrowing mode/JSON value), DEV-0010
  (deterministic nsid does not follow upstream random bytes), DEV-0011 (fixing the upstream defect of dangling footnote
  relationships), and the low-level `SubdocFragment` not merging parts; see
  `compatibility.md` §5.

## Post-Freeze Addendum (2026-09-26)

`RenderValue::Subdoc` was subsequently changed to carry a `SubdocFragment` validated through strict XML/DTD/namespace
checks, retaining truthiness, safe identity, and non-string identity through a RichMarkup wrapper. `new_subdoc_from_reader`
/ `new_subdoc_from_bytes` and the high-risk paths for custom properties,
SmartArt, VML, footnotes, multi-section, namespace, and missing-numbering/restart belong to the compatibility hardening
after the original five P6 fixtures. They are accepted by entry-equivalence regressions, dynamic DOCX integration tests,
and standalone Python semantic probes, and are not retroactively added to the original 78-item render / 76-item recovery
denominators; the only authoritative list of boundaries still open is DEV-0008/0010/0011/0014 in compatibility.md.
