# ADR-002: XML Editing Model — Upstream-Isomorphic Bounded String Pipeline plus Lenient Healing Parser

- Status: Accepted (P0)
- Decision date: 2026-09-24

## Context

An audit of upstream 0.20.2 confirmed (`third_party/docxtpl-0.20.2/docxtpl/template.py`):

- `patch_xml` consists entirely of **bounded** string regex transformations (restricted to inside jinja tags or inside a single carrying element), with nesting guards such as `<w:p[ >](?:(?!<w:p[ >]).)*`.
- Merging split tags (steps A/B) deliberately deletes the XML markup on both sides of the tag, producing a **locally invalid** string.
- Rendering defaults to `autoescape=False`: context values are inserted into XML **as-is**; illegal characters rely on the lenient `etree.fromstring(xml, parser=XMLParser(recover=True))` parsing in `fix_tables` to "heal", after which python-docx re-serializes.
- Therefore the observable upstream output = the result of string surgery parsed by libxml2 recovery + re-serialization by lxml.

A pure XML-tree implementation would produce "more correct but different" output (for example, bare `&` in values escaped ahead of time, or merged tags not breaking the structure), causing documented deviations to proliferate and violating the compatibility priority set by the planning documents.

## Decision

Adopt a pipeline **isomorphic** to upstream (`docxtpl-template`):

1. Extract `w:body` (including the xmlns declarations visible on the root) as a string, replicating the behavior of `lxml.tostring(body)`.
2. In `docxtpl-compat`, port the 13 bounded steps of patch_xml one by one with fancy-regex
   (lookahead/lookbehind/guards kept as-is; the compatibility matrix is consolidated by semantic groups A–K).
3. MiniJinja rendering (default autoescape=false, aligned with upstream).
4. `resolve_listing` is likewise ported with bounded regexes.
5. **Lenient parsing** (`docxtpl-xml`, in-house) simulates the key behaviors of libxml2 recovery on this corpus:
   - A bare `&` in text (not a legal entity) is kept as character data and escaped when serialized;
   - A `<name` form followed by legal name characters is treated as an element start (a value injecting `<b>` produces a real element);
   - Mismatched closing tags and dangling structures are discarded/repaired by the parser, with diagnostics recorded (not silently swallowed);
   - The exact boundaries are pinned down by oracle fixtures (the r2_value_* series).
6. Tree-level fixes `fix_tables` / `fix_docpr_ids` match by namespace URI (equivalent to upstream's `nsmap` usage).
7. Errors carry the part name, line number, and a `docx_context`-style text snippet (aligned with upstream `exc.docx_context`).

## Relationship to Quality Spec §4.2

What the spec prohibits is "unbounded string replacement". Every step of this pipeline has an explicit scope (inside a tag / inside a single element / inside a single w:t), and structural validation and diagnostic output are performed by a strict/lenient dual-mode parser; it does not rely on silent "it seems to open" recovery. All healing actions are logged.

## Consequences

- Long-tail recovery behaviors not pinned down by fixtures are handled conservatively and registered as deviations (DEV-0002).
- A future "strict mode" (eager escaping, rejection of illegal input) will be designed separately as an explicit option and is not counted toward the compatibility rate.
