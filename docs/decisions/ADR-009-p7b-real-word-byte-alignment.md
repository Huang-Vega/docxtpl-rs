# ADR-009: P7b Byte Alignment with Real Word Templates (patch input tree round-trip / package-level save normalization / footnote raw-byte round-trip / cross-tree namespace merging)

Date: 2026-09-26
Status: Accepted
Phase: P7b (docxtpl 0.20.2 upstream real-template corpus, 16 new render fixtures)

## Context

The 85 render fixtures of P0–P7a are all programmatically synthesized templates: their XML bytes had already passed
through lxml normalization (single-quote declaration, LF, no inter-element indentation), isomorphic to the intermediate
form of the upstream rendering pipeline. Running the 16 real Word templates under `tests/templates/` in the docxtpl
0.20.2 repository (Word 2016 save form: double-quote declaration + CRLF, inter-element indentation, paragraph-local
namespaces, rels registered as Overrides, customXml parts, etc.) produced diffs that exposed 6 blocking difference
families (probe logs in `.trae/p7b_probe/`):

- **B1**: save-time package-level normalization differences — the rels Override vs Default placement in
  `[Content_Types].xml`, the declaration lexicon of all rels, and the declaration/indentation form of known XmlParts
  such as styles/settings/numbering;
- **B2**: the input fed to `patch_xml` for the body/headers/footers is not the raw on-disk bytes but the serialization
  result of the python-docx oxml tree (entity decoding, indentation stripping);
- **B3**: the `{_% %_}` literal escape restoration wrongly restores `{_%` to `{%_` (a one-line bug);
- **B4**: real template paragraphs carry a local `xmlns:wp14` (same URI as the root `xmlns:w14` but a different
  prefix), while upstream output uniformly uses the ancestor prefix `w14` and discards the local declaration;
- **B5**: the Word-original declaration/CRLF form of untagged footnote parts is broken by this side's tree round-trip;
- **B6**: suspected incompatibility with eastAsia fonts and space/Tab RichText (after probing, proven already natively
  supported, **zero changes**).

This ADR records the porting decisions for B1–B5; the corpus consists of the LGPL-2.1 test templates of docxtpl 0.20.2,
copied into `tests/fixtures/sources/p7b_*.docx`.

## Upstream Facts Pinned Down by Probes (docxtpl 0.20.2 + python-docx 1.2.0 + lxml 6.1.1)

1. **Three patch_xml input paths** (template.py L289-461): the body uses
   `etree.tostring(body)`; headers/footers use `etree.tostring(parse_xml(part.blob))`; footnotes use
   `part.blob.decode()`. The first two come from the python-docx oxml parse tree
   (`opc/oxml.py` L21: `remove_blank_text=True, resolve_entities=False`) — inter-element indentation
   whitespace is stripped, attribute entities are decoded to literal characters (`&quot;`/`&apos;` inside jinja
   expressions no longer remain), and empty-element lexicon is normalized.
2. **`serialize_part_xml`** (oxml.py) =
   `etree.tostring(elm, encoding="UTF-8", standalone=True)` →
   `<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n`
   (**single quotes**, always one LF after the declaration; no trailing line when there is no XML content).
3. **B3**: the literal restoration in upstream template.py L323-328 is
   `xml = xml.replace("{_%", "{%")` (this side mistakenly wrote `{%_`).
4. **B1(a) CT rebuild**: `PackageWriter._write_content_types_stream`
   → `_ContentTypesItem.from_parts` (pkgwriter.py L80-99) unconditionally pre-populates `Default rels`/`Default xml`
   on every save, walks the parts calling `_add_content_type`: extensions hitting the python-docx
   `docx/opc/spec.py` `default_content_types` table land in Default, the rest land in Override; rels parts are not
   enumerated in parts. In real Word templates, the rels Overrides for `/_rels/.rels` and `/word/_rels/*.rels`
   and the application/xml Override for `customXml/item1.xml` disappear after saving (substantiated by the hf_entities
   expected CT: two Default rels/xml entries at the start, only 13 Overrides). On `to_xml`, Default entries are sorted
   by ext and Override entries by partname in ASCII order.
5. **B1(b) rels are always rewritten**: the root rels and all attached rels (including
   `customXml/_rels/item1.xml.rels`) are rewritten by PackageWriter from the `Relationships` model into lxml form; the
   relationship content is identical, with differences only in declaration lexicon (double quotes + CRLF, missing
   standalone, trailing newline).
6. **B1(c) known XmlParts are always re-serialized**: XmlPart subclasses registered by the python-docx PartFactory
   (document/header/footer/core **/styles/settings/numbering**) save with `XmlPart.blob` =
   `serialize_part_xml(element)` (part.py L221), rewritten even when rendering did not touch them; the order template
   substantiates that settings.xml/styles.xml have identical content differing only in the declaration (double-quote
   CRLF vs single-quote LF). Other parts
   (fontTable/webSettings/theme/footnotes/endnotes/comments/
   customXml, etc.) are generic Parts whose `blob` returns the original loaded bytes.
7. **B5 footnotes are a generic Part**: footnotes+xml is not registered in PartFactory, and `render_footnotes` patches/
   runs jinja directly on `part.blob.decode()` and writes back to `part._blob` — the template's Word double-quote
   declaration passes through verbatim; the comments/footnotes_real expected output substantiates the declaration as
   `version="1.0" ...` (double quotes).
8. **The Jinja2 lexer eats CR**: the lexer `tnewline = \r\n|\r(?!\n)|\n` uniformly produces NEWLINE tokens, so rendered
   output is always LF — the CRLF after the declaration in raw footnote bytes becomes LF after one jinja round-trip
   (substantiated by length 3452→3451). lxml tree output has no CR anyway, so this normalization is unobservable on the
   body/story paths.
9. **B4 cross-tree re-grafting merges namespaces**: body rendering **appends one by one** the body children of
   `parse_xml(rendered)` into the original document tree (`replace_children`). When lxml/libxml2 moves an element across
   trees, if the URI of an element nsDef is already bound on the target tree's ancestor axis (with **any prefix**), that
   nsDef is discarded and the prefixes of the element name and all descendant elements/attributes are rebound to the
   ancestor prefix:
   `<w:p xmlns:wp14="U" w14:a="1" wp14:b="2"><w:c wp14:z="3"/></w:p>`
   re-grafted under a document with `xmlns:w14="U"` →
   `<w:p w14:a="1" w14:b="2"><w:c w14:z="3"/></w:c>`. A plain
   parse/tostring does **not** trigger merging (probes substantiate that redundant declarations are retained verbatim) —
   therefore the header/footer paths that output a whole-tree parse stay lexical.

## Decision

1. **Fixture mechanism** (tests/fixtures/generate.py): `@fixture` gains
   `source`/`source_upstream` parameters; when source is not None, the docx is copied from
   `tests/fixtures/sources/` instead of being built programmatically, and the manifest records
   `"source": "docxtpl-0.20.2:tests/templates/<original name>"`. Add 16 new
   `p7b_*` fixtures (numbers 106-121), of which word2016/cellbg/richtext_if/
   eastasia use `context_kind="python"`, with 4 Rust arms replicated 1:1 on the oracle_diff.rs side
   (RichText::text/text_with + props(), cellbg built with RenderValue::array/object). The LGPL-2.1 corpus is stored with
   the repository.
2. **B2/B1(c) share `normalize_part_xml(src, part_name)`**
   (publicly exported from docxtpl-template): `parse_strict` →
   `strip_blank_text()` → always output single-quote declaration + LF +
   `serialize_subtree(root)`. The pre-patch input for the body/stories and the save-time unconditional normalization of
   styles/settings/numbering share the same form; failure to parse a well-formed template is reported as an XML error
   (with the part name), consistent with upstream failing when opening the document. Keeping the declaration in
   preprocessing is necessary: stripping it would make the tree leniently re-parsed after rendering lose `has_decl`,
   leaving the final output without a declaration line (which once caused 96 existing regressions).
3. **B5 footnote switch**: `render_part_string` gains
   `normalize_input: bool` — true for the body/stories; false for footnotes
   (raw bytes enter the patch directly, preserving the template's declaration lexicon). The same entry point uniformly
   performs `.replace("\r\n","\n").replace('\r',"\n")`, aligned with the Jinja2 lexer's tnewline normalization
   (observable only on the footnote path).
4. **B1(a) opc CT rebuild**: docxtpl-opc adds
   `DEFAULT_CONTENT_TYPES` (1:1 port of spec.py default_content_types:
   bin×3 printerSettings, bmp/emf/fntdata/gif/jpe/jpeg/jpg/png/rels/
   tif/tiff/wdp/wmf/xlsx/xml), the `OPC_RELATIONSHIPS_CT`/`XML_CT` constants, and
   `ContentTypes::rebuild_from_parts(parts)` (pre-populates rels/xml Defaults, puts the rest into Default when the
   lowercase extension hits the table and otherwise Override, replacing wholesale; sorting remains in `to_xml`).
5. **B1(b/c) Package normalization APIs**: `Package::rebuild_content_types`
   enumerates all parts except directories/CT/`.rels` and rebuilds the view according to the current CT;
   `Package::normalize_relationships` rewrites the root rels and all attached rels into the canonical
   `Relationships::to_xml()` bytes. Before saving, docxtpl-rs upgrades `canonicalize_content_types` to three steps:
   `normalize_known_xml_parts` (tree round-trip for the styles/settings/numbering **whitelist
   CTs**, not written when bytes are unchanged; document/hdr/ftr/core are already rewritten in the rendering pipeline,
   generic Parts continue to pass through) → `normalize_relationships` →
   `rebuild_content_types` + writing back CT differences. render() and
   RenderSession::finish share this entry point.
6. **B4 serialization-time re-graft rebinding** (docxtpl-xml serialize.rs):
   the `retain_redundant_ns=false` path (body/Subdoc fragments) gains
   `ancestor_uri_prefix(node, uri)` — looks up (near to far) the **effective** bound prefix of the URI along the
   ancestor axis (recursively handling inner same-prefix-different-URI shadowing): when an element nsDecl shares a URI
   with any ancestor prefix, the declaration is omitted; the prefixes of element and attribute names are rebound by URI
   to the ancestor prefix. `retain_redundant_ns=true`
   (whole-tree parse/tostring of headers/footers + injected fragments) keeps all lexicon
   (ADR-006). The old same-prefix-only query `ancestor_ns_binding` is deleted.
7. **B3 one-line fix** (docxtpl-template render.rs):
   `.replace("{%_", "{%")` → `.replace("{_%", "{%")`.
8. **B6 zero changes**: eastAsia font prefixes and pure-space/Tab RichText were verified byte-by-byte via 4
   python-context fixtures to be natively supported.

## Impact

- New public API: `docxtpl_template::normalize_part_xml`;
  docxtpl-opc `ContentTypes::rebuild_from_parts` and
  `Package::rebuild_content_types`/`normalize_relationships`
  (the default content-type table is a crate-private constant).
- No new third-party dependencies (the spec.py default table is ported as constants).
- Acceptance: **all 101 render fixtures match the Python oracle**
  (97 byte/c14n double matches per part + 4 error categories: Syntax×2, Image×1,
  Value×1), and all 16 p7b fixtures MATCH; golden counts are 101 (full_patched) /
  99 (pre_recover; of the 101, r2_syntax_error/p4_img_bad export only
  patched); zero regressions among the 85 existing fixtures.
- No new DEV items in this phase: generic Part blob pass-through, ns rebinding, etc. are all upstream-aligned behavior;
  existing exclusions (DEV-0005 autoescape+rich values, DEV-0008 Subdoc docpath-only, the custom jinja_env rejection
  list) continue to apply.
- During P7b implementation there was no local Office/LibreOffice spot check on a real machine (the corpus comes from
  the upstream repository, and byte diffs are pinned per part); P7d subsequently added representative real-machine spot
  checks of LibreOffice 8/8 and Microsoft Word 10/10; see `docs/p7-compatibility-report.md` for details.

## Post-Freeze Addendum (2026-09-26)

Subsequent hardening such as Jinja/value, XmlPart UTF-16/32, Subdoc reader/bytes, CLI, and accessibility does not belong
to this ADR's historical denominators of 101 render / 101/99 golden; their evidence and remaining deviations are
uniformly registered in `docs/compatibility.md`, and the P7b conclusion of "byte-exact per-part equality with real Word
templates" is not expanded on that basis. In the current state DEV-0005 is closed, DEV-0008 retains only the
current-document-borrowing/JSON-Subdoc boundary; a Rust-native environment configurator is provided, while Python
`jinja_env`/extension objects are still rejected.
