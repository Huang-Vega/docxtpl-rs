# ADR-004: Resource Limits, Security, and Error Policy

- Status: Accepted (P0; P1 numerical calibration: see docs/security-limits.md)
- Decision date: 2026-09-24

## Decision

1. Upstream applies **no limits whatsoever** to ZIP/XML input; limits are a security enhancement of this project and
   constitute a documented deviation from upstream (on the security side). The entire corpus is within the limits, so
   diffing is unaffected.
2. Default limits (`docxtpl-opc::PackageLimits`, values after P1 calibration):
   - `max_entries` = 1 000 (number of ZIP entries)
   - `max_entry_uncompressed` = 32 MiB (decompressed size of a single entry)
   - `max_total_uncompressed` = 128 MiB (total decompressed size)
   - `max_compression_ratio` = 200 (compression ratio; blocks zip bombs)
   - `max_output_size` = 128 MiB (size of the written-out package)
   - XML nesting depth ≤ 512 (docxtpl-xml parser)
3. URI validation: reject absolute paths, `..` parent-directory escapes, backslashes, and drive letters; duplicate
   entries are detected under three normalizations — exact, case-folded, and percent-decoding-folded — and a conflict is
   an error.
4. Error policy: all foreseeable failures go through `Result`; unwrap/expect/panic are forbidden. Errors are layered
   (Input/Zip → OPC → XML → Template → Context → Limit → Output), retain their source, and carry the part and position.
   Public error categories remain stable; concrete texts may evolve.
5. External entities (DTD/XXE) are disabled; lenient (recover) parsing is used only to heal rendering results, and all
   healing actions record diagnostics.

## Review

- Samples, measured maxima, and limitations of the P1 corpus are recorded in security-limits.md; review when the set of
  real templates is expanded.
- A high-severity security issue triggers a release freeze (code spec §6).

## Post-Freeze Addendum (2026-09-26)

- The facade additionally limits compressed input DOCX to 128 MiB; the rendering pipeline uses a 64 MiB budget for
  each controlled output/intermediate buffer, MiniJinja evaluation runs with 10 000 000 fuel per evaluation, and CLI
  JSON input is limited to 64 MiB. Allocation-time XML serialization, layered listing expansion, image replacement, and
  expansion-type filters are all checked while the result is being constructed, rather than only after a full
  allocation.
- Intermediate collections such as `split`/`urlencode`/`join` are additionally subject to the 524 288-item budget; even
  if the final string has not reached 64 MiB, a call may fail early because an intermediate collection is too large. The
  values above are per-path budgets, not a 64 MiB cap on whole-process RSS or peak memory.
- CLI JSON files are subject to the 64 MiB input limit; the programmatic `render`/`render_ctx` context has no
  equivalent byte limit. Streaming of `tojson` string escaping is limited, but the intermediate value tree is still
  materialized first, so callers must still bound the total volume of untrusted programmatic input.
- InlineImage filename/title/descr are validated against XML 1.0 characters before the XML is finalized, and the 64 MiB
  budget is enforced on the aggregate post-escaping size; shape_id and gridSpan extremes use saturating arithmetic.
- JSON integers are preserved with exact fidelity only within signed i128; out-of-range values return
  `InvalidArgument` — they are not converted to f64 or handed to unsigned-arithmetic paths that may silently overflow
  (DEV-0016).
- `docs/security-limits.md` is the current source of truth for the numbers; this ADR retains the original P0/P1
  package-level decision context.
