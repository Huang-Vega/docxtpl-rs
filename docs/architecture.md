# docxtpl-rs architecture

> Consistent with the planning document §3; this file records the actual shape
> after the P0 decisions (ADR-002/-003/-004).

## Data flow

    DOCX + Context
      │
      ▼
    docxtpl-opc: ZIP/OPC reading (limits, URI validation, part/rels/content-types index)
      │
      ▼
    docxtpl-rs facade: locate the main document part, extract w:body (including inherited xmlns)
      │
      ▼
    docxtpl-template:
      patch_xml (docxtpl-compat regexes, 13 bounded transforms)
      -> MiniJinja rendering (default autoescape=false)
      -> resolve_listing
      -> docxtpl-xml lenient (recover) parse healing (diagnostics recorded)
      -> fix_tables / fix_docpr_ids (namespace URI matching)
      -> serialize and graft back into document.xml
      -> render header/footer, core properties, footnotes in upstream order
      │
      ▼
    docxtpl-opc: part updates -> ZIP write-out (unmodified parts kept as-is) -> package integrity check

## Module boundaries and dependency direction

    docxtpl-rs (facade) -> docxtpl-template -> docxtpl-xml / docxtpl-compat
                        docxtpl-rs       -> docxtpl-opc -> quick-xml (only for parsing rels/content-types)
    docxtpl-cli -> docxtpl-rs

- opc knows nothing about Jinja; xml never touches ZIP; compat only collects
  rules with upstream evidence (upstream regexes ported one by one).
- tests/oracle (Python) is test infrastructure only; runtime crates do not
  depend on it.
- Each render has independent state: DocxTemplate is reusable, and render
  returns a new RenderedDocument.

## Key invariants

1. Unmodified parts are preserved as-is (DEV-0004).
2. Rendered output must pass lenient parsing plus package validation; every
   healing action produces a diagnostic.
3. Error messages carry the part name, line number, and text context (aligned
   with upstream docx_context).
4. Scoping of identifiers such as wp:docPr id and rId: global renumbering in
   the body applies only to docPr (aligned with upstream); rels are modified
   only by explicit features (P4+).
