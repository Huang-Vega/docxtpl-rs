# ADR-011: P8 Streaming I/O for Large Documents and Resource Budgets

Date: 2026-09-26  
Status: Accepted  
Phase: P8

## Context

The P7 defaults of a 128 MiB package budget and a 64 MiB rendering buffer suit conventional templates but cannot cover
large reports. The file entry point previously read the compressed DOCX fully into a `Vec<u8>`, after which OPC held all
decompressed parts; saving also first constructed the complete output ZIP in memory, so peak memory included two
unnecessary whole-package copies.

## Decision

1. Document-related default budgets are raised to 600 MiB: compressed input, single-entry decompression, total
   decompression, the single rendering XML buffer, and final output; the ZIP entry cap is raised to 6000.
2. The compression ratio of 200, MiniJinja 10 000 000 fuel, and the 64 MiB CLI JSON guard are retained; no default
   unlimited mode is provided.
3. Add `ResourceLimits`, allowing callers to tighten input, `PackageLimits`, rendering XML, and fuel; external Subdocs
   inherit the main package's `PackageLimits`.
4. `DocxTemplate::open` stores the source path rather than the compressed package bytes; the file is reopened for every
   render. Because `from_reader`/`from_bytes` have no stable reopen source, they continue to retain the input bytes.
5. For a new path, `Package::save` writes the ZIP to a temporary file in the target directory and persists it on
   success; when overwriting an existing path it truncates directly and streams the write, to stay compatible with
   Windows callers that still hold the target handle. Both paths eliminate the complete output `Vec<u8>`; the memory
   semantics of `write_to` and explicit `to_bytes` remain unchanged.
6. `Package::open(path)` materializes immediately only the OPC structural metadata (Content Types and `.rels`). Other
   parts save the source ZIP entry index, decompressing and caching on the first `Part::bytes()`; read failures are
   returned lazily through `Result`. Unmodified parts are written out by raw-copying the original compressed entries,
   without being materialized or recompressed just to save. `from_reader` continues to read eagerly because the reader
   lifetime cannot be guaranteed.

## Impact and Remaining Boundaries

- A file template must remain accessible after `open`; if its content is replaced, subsequent renders read the new
  content and re-run the input-size and OPC checks.
- Accessed parts cache their decompressed bytes without automatic eviction; the XML DOMs needed for rendering, accessed
  media, and multiple bounded buffers may still reside simultaneously. Lazy storage therefore significantly lowers the
  peak for opening and pass-through saves, but no promise is made that render peak RSS is below 600 MiB.
- Callers needing an in-memory byte result explicitly choose `RenderedDocument::to_bytes` and accept one additional
  complete-output allocation; large-document services should prefer `save` or `write_to`.
