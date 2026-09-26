# Security limits (security-limits)

> Status: the P7c corpus review was completed on 2026-09-26; the values apply
> to the default configuration.
> Changes to this file must update the tests in the same PR (code spec
> §1.3 / §6).

## Input limits (docxtpl-opc::PackageLimits, defaults)

| Item | Default | Description |
|---|---|---|
| `max_entries` | 6,000 | Number of ZIP entries |
| `max_entry_uncompressed` | 600 MiB | Decompressed size of a single entry |
| `max_total_uncompressed` | 600 MiB | Total decompressed size of all entries |
| `max_compression_ratio` | 200 | Decompressed/compressed (zip-bomb interception; stored entries are not limited) |
| `max_output_size` | 600 MiB | Written package size (enforced by a counting write wrapper) |

The facade additionally limits compressed input DOCX files to 600 MiB. The
render pipeline applies a 600 MiB budget to each controlled string or output
buffer, and MiniJinja gets 10,000,000 fuel per template evaluation; exceeding
the limits returns an error. The 600 MiB budget is applied during allocation to
MiniJinja captured output, document/story/core XML serialization and
normalization, introspection subtree serialization, image placeholder
replacement, and each layer of regex replacement and control-character
expansion in `resolve_listing_limited`, avoiding fully allocating an expanded
result before checking its length. `join` and other expansion-type filters also
build their output via bounded appending.

Filters that must collect intermediate collections, such as `split`,
`urlencode`, and `join`, are additionally subject to a 4,915,200-item budget
(derived conservatively from 600 MiB / 128 B per item); therefore, even if the
final text could be smaller than 600 MiB, an extreme number of intermediate
items still returns a limit error early. Template error context is collected
only as a streaming fixed window and is subject to the same class of budget at
construction time.

These are per-path output/intermediate-allocation upper bounds, not a uniform
600 MiB guarantee on process RSS or peak render memory; the XML DOM, regex/
template-engine internals, multiple simultaneously live bounded buffers, and
dependency-library allocations still add up. For package-level total-
decompression and write-out limits, see the table above.
The CLI reads at most 64 MiB of the context file before JSON parsing; direct
invocation and the `render` subcommand use the same limit.

The CLI limit constrains only the JSON file it reads; programmatic contexts
that library callers pass directly to `render`/`render_ctx` have no equivalent
input-byte cap. Although the custom `tojson` applies the 600 MiB budget to the
final `JsonOutput` and streams string escaping while writing, it still
materializes an intermediate `PythonJsonValue` tree first. Callers handling
untrusted programmatic contexts must still impose a total-size limit outside
the library.

The file-path entry point does not keep the compressed DOCX resident as a
second `Vec<u8>`; each render reopens the source file. During save, for a new
path the ZIP is streamed directly to a temporary file in the same directory and
persisted on success; when overwriting an existing file it is streamed with
direct truncation, to stay compatible with target handles already open on
Windows. Neither path constructs the full output bytes first. An OPC package
opened from a path reads only the central directory, Content Types, and
relationship files immediately; the body, media, and other ordinary parts are
decompressed and cached on first access. On save, unmodified entries raw-copy
the original compressed data directly and are not materialized for write-out.
Because the generic `Read + Seek` entry point has no long-lived, reopenable
file backend, it still uses eager reading. 600 MiB is a capacity gate, not a
peak-RSS guarantee: parts needed for rendering, the XML DOM, and multiple
buffers may still reside at the same time. Library callers can tighten the
input, package, rendered-XML, and fuel limits via `ResourceLimits`;
`PackageLimits` continues to provide low-level per-item configuration.

InlineImage filename/title/descr are checked for XML 1.0-legal characters before
relationships or XML are finalized, and a 64 MiB budget is enforced on the
aggregated size at the two attribute landing points after attribute escaping;
they return `ImageError::InvalidXmlMetadata` or `ImageError::MetadataTooLarge`
respectively. Extreme `shape_id` and table `gridSpan` calculations use
saturating arithmetic to avoid panics from integer overflow; this does not
change the framing above of per-path rather than process-wide memory caps.

## XML limits (docxtpl-xml)

| Item | Default |
|---|---|
| Maximum nesting depth | 512 |
| External entities/DTD | Disabled (XXE) |

## URI rules

- Rejected: absolute paths (drive letter / leading `/`), `..` segments,
  backslashes, empty entry names.
- Three duplicate-detection views: exact, case-folded, and percent-decoding-
  folded.
- After resolution, rels internal targets must fall inside the package
  (validated by `validate()`; a dangling target is an error).

## Known security tests (since P1)

- Zip bombs (high-compression-ratio entries), entry-count overflow, total-
  decompression overflow, single-entry overflow.
- Illegal paths (`../evil.txt`, absolute paths), duplicate entry names,
  truncated ZIP.
- Deeply nested XML, illegal entities, oversized text nodes.

## P7c measured basis and review

The 127 DOCX files under `tests/fixtures/templates` were read one by one via
the ZIP central directory:

| Metric | Corpus maximum | Corresponding sample |
|---|---:|---|
| Compressed file | 38,750 B | p5_hf_multi.docx |
| Entry count | 23 | p7b_footnotes_real.docx |
| Single-entry decompressed size | 438,131 B | p4_combo_rich.docx |
| Total decompressed size | 833,014 B | p5_hf_multi.docx |
| Single-entry compression ratio | 32.156 | p4_combo_rich.docx |

The defaults still leave substantial headroom for the corpus, while tightening
the entry-count and memory-related caps relative to the ADR-004 initial values.
This corpus consists entirely of small generated templates and cannot represent
real large documents; before accepting large real templates, expand the samples
and review these values. The fields of `PackageLimits` can be adjusted through
the low-level package API.

P7c additionally uses proptest to cover random/truncated ZIP, strict/recover
parsing of arbitrary UTF-8 and XML-like input, and marker-heavy `patch_xml`;
each property runs 256 or 512 cases, ensuring error paths return controlled
errors instead of panicking. The review and performance command is
`python tests/p7c_audit.py`.
