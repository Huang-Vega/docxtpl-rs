# Migration guide

## 1.2 to 1.3

Version 1.3.0 is additive relative to the frozen 1.2 public API. Existing
rendering, editing, cancellation, and output entry points retain their behavior;
no source migration is required. Applications may adopt the new capabilities
independently:

- use `EditableStorySelection` and `for_each_editable_story` for body, header,
  footer, footnote, endnote, and comment editing;
- use the bounded regex and capture-replacement methods on `RunTextIndex`, with
  optional `RunFormatOverrides`;
- attach an explicitly owned `PreparedTemplateCache` when preprocessing should
  persist across template instances;
- use `RenderControl` and the `RenderLimits` alias for unified cancellation,
  deadline, and resource-budget terminology;
- inject a blocking executor into `AsyncRenderDispatcher` when async scheduling
  and bounded in-flight backpressure are required.

The old `StoryScope`, `StoryKind`, `CancellationToken`, `CancellationError`, and
`ResourceLimits` types remain available. In particular, 1.3 does not add
variants to the exhaustively matchable 1.2 story enums.

## 1.2 editing API

The 1.2 line added an opt-in editing path for applications that need
to post-process a rendered document without first serializing and reopening an
intermediate DOCX:

```rust
use docxtpl_rs::{DocxTemplate, RenderOptions};
use serde_json::json;

let template = DocxTemplate::open("template.docx")?;
let mut rendered = template.render(&json!({"name": "Vega"}), &RenderOptions::compat())?;
rendered.edit_package(|package| {
    let bytes = package
        .part("word/document.xml")
        .expect("a rendered DOCX has a main document part")
        .bytes()?
        .to_vec();
    package.set_part_bytes("word/document.xml", bytes)?;
    Ok(())
})?;
rendered.save("output.docx")?;
# Ok::<(), docxtpl_rs::Error>(())
```

Entering `edit_package` marks the rendered document as edited even when the
callback later returns an error. All subsequent save, write, and in-memory
serialization methods validate OPC integrity before producing output. The
low-level callback does not roll back partial changes; use the transactional
pipeline when that behavior is required:

```rust
use docxtpl_rs::{FailurePolicy, RenderOptions};

let mut rendered = template.render(&context, &RenderOptions::compat())?;
let report = rendered.postprocess(|pipeline| {
    pipeline.pass("required-edit", FailurePolicy::Abort, |transaction| {
        let bytes = transaction
            .part("word/document.xml")
            .expect("main document")
            .bytes()?
            .to_vec();
        transaction.set_part_bytes("word/document.xml", bytes)?;
        Ok(())
    })?;
    pipeline.pass(
        "best-effort-edit",
        FailurePolicy::WarnAndRollback,
        |_transaction| Ok(()),
    )?;
    Ok(())
})?;
assert_eq!(report.passes.len(), 2);
# Ok::<(), docxtpl_rs::Error>(())
```

The transaction snapshots only parts touched through its mutation methods.
Newly added parts are removed on rollback; relationship owners and the parsed
Content Types/root-relationship views are restored together with their XML
parts. Successfully completed earlier passes stay committed when a later pass
fails.

Multiple operations that traverse the body, headers, or footers should share a
single story edit operation. Mutable DOM access marks that story for one final
bounded serialization; read-only inspection does not rewrite it:

```rust
use docxtpl_rs::{FailurePolicy, StoryScope};

rendered.postprocess(|pipeline| {
    pipeline.pass("story-edits", FailurePolicy::Abort, |transaction| {
        let report = transaction.for_each_story(
            StoryScope::BodyHeadersFooters,
            |story| {
                // Run all edits for this part against story.document_mut().
                // Calling document_mut() more than once still serializes once.
                let _document = story.document_mut();
                Ok(())
            },
        )?;
        assert_eq!(report.parsed_parts, report.serialized_parts);
        Ok(())
    })?;
    Ok(())
})?;
# Ok::<(), docxtpl_rs::Error>(())
```

`StoryScope::BodyHeadersFooters` visits the main document first, then internal
headers, then internal footers. Duplicate targets, external relationships,
missing targets, and empty story parts follow the existing render-time story
enumeration boundaries.

Post-processing passes can register a path-backed image once and relate it to
one or more body/header/footer owners without manually editing `.rels` or
`[Content_Types].xml`:

```rust
rendered.postprocess(|pipeline| {
    pipeline.pass("logo", FailurePolicy::Abort, |transaction| {
        let media = transaction.register_media_path("logo.png")?;
        let body_rid = transaction.relate_image("word/document.xml", &media)?;
        let header_rid = transaction.relate_image("word/header1.xml", &media)?;
        // Use the returned ids while editing the corresponding story XML.
        let _ = (body_rid, header_rid);
        Ok(())
    })?;
    Ok(())
})?;
# Ok::<(), docxtpl_rs::Error>(())
```

Registration deduplicates media by SHA-1 across the package and the current
pass. Exact relationships are idempotent, and all added parts, relationships,
and Content Types declarations are removed if the pass rolls back.

Long-lived rendered documents can inspect and release clean lazy part buffers
before the final write:

```rust
let before = rendered.residency();
let eviction = rendered.evict_clean_part_caches();
assert!(eviction.after.resident_bytes <= before.resident_bytes);
```

Eviction never discards modified data, in-memory-created parts, or file-backed
media. Evicted source-ZIP parts are transparently reloaded if read again. The
reported residency covers part-content buffers, not allocator overhead or the
template preprocessing cache, and therefore is not a process RSS measurement.

File-path saves are now atomic for both new and existing destinations. The
package is validated and written under `PackageLimits` into a synchronized
temporary file in the destination directory before replacement. Validation,
source-file verification, output-limit, temporary-file, or replacement errors
therefore leave an existing destination unchanged. On Windows, callers must
close any open destination handle before saving; a sharing violation now fails
safely instead of falling back to truncating the destination.

Low-level `Package` add and replace operations reject changes that exceed
`max_entries`, `max_entry_uncompressed`, or `max_total_uncompressed`. Lazy ZIP
parts contribute their declared uncompressed size without being materialized.

Bookmarks and internal links can be created in the same bounded story edit:

```rust
transaction.for_each_story(StoryScope::Body, |story| {
    let target_run = /* a w:r NodeId from story.document() */;
    let bookmark = story.get_or_create_bookmark(target_run, "defect RET-01008")?;
    let linked_run = /* another w:r NodeId */;
    story.attach_internal_link(linked_run, &bookmark)?;
    Ok(())
})?;
```

Names are normalized to Word-safe, at-most-40-character values and ids use the
first non-negative gap. Repeating either operation is idempotent. Before story
serialization, duplicate names/ids, unmatched start/end markers, nested links,
and links to missing bookmarks are rejected so the enclosing pass can roll
back.

Existing pictures can receive an external click target without rebuilding the
Drawing. Register the relationship before entering the story edit so the
editor can verify that the rId belongs to that story:

```rust
let rid = transaction.relate_external_hyperlink(
    "word/document.xml",
    "https://example.test/video",
)?;
transaction.for_each_story(StoryScope::Body, |story| {
    let drawing = /* a w:drawing NodeId from story.document() */;
    story.attach_drawing_external_link(drawing, &rid)?;
    Ok(())
})?;
```

The operation writes the same `a:hlinkClick r:id` under `wp:docPr` and each
picture `pic:cNvPr`. Exact relationships and repeated attachment are reused;
an invalid owner/rId or duplicate click element fails the pass transaction.

Applications that replace a large existing media part can use
`FilePartSource::snapshot` with `Package::set_file_backed_part`. The file is
streamed during ZIP output and its metadata and SHA-1 digest are rechecked.

The 1.2 image probe also accepts marker-valid JPEG files that begin with SOI
and contain a supported SOF but have no JFIF or Exif application segment. Such
files use 72 dpi for native-size conversion. Existing JFIF and Exif inputs keep
their previous DPI behavior, so applications no longer need to inject a
temporary JFIF segment and restore the original media bytes after rendering.

Call `save_with_report` when production diagnostics need structured phase and
ZIP metrics. The ordinary `save`/`save_with_options` APIs retain their previous
return types and do not allocate a detailed report:

```rust
use docxtpl_rs::WriteOptions;

let report = rendered.save_with_report(
    "output.docx",
    &WriteOptions::compatible(),
)?;
assert_eq!(
    report.package_write.raw_copied_parts
        + report.package_write.rewritten_parts,
    report.package_write.total_parts,
);
println!(
    "wrote {} bytes in {:?}",
    report.package_write.output_bytes,
    report.zip_write_elapsed,
);
# Ok::<(), docxtpl_rs::Error>(())
```

`RenderReport` includes the completed pass reports accumulated on the rendered
document. Byte counters are aggregate ZIP/source counters, not a process-wide
memory measurement; peak RSS remains an application benchmark responsibility.

## 1.0 release line

`1.0.0` is the first stable public compatibility baseline of this repository;
`1.0.0-rc.1` began the release-candidate line.
Earlier development snapshots were never published or tagged as supported
releases, so there is no supported pre-RC migration path. The API and behavior
described here apply to the stable `1.0` release line.

## Minimum Rust version

The MSRV is Rust 1.85. Applications should keep at least one `1.85.0` check job
in CI and verify application dependency resolution using the committed
`Cargo.lock`.

## Public API baseline

- `RenderOptions` does not implement `Copy`. When reusing it across scopes,
  borrow it or call `clone()` explicitly; `autoescape()` borrows `&self`.
- `InlineImage` exposes public `title` and `descr` fields. Prefer
  `with_accessibility()` to avoid depending on the field set of a struct
  literal.
- `docxtpl_rs::Error` includes `InvalidXmlEncoding`;
  `docxtpl_rich::ImageError` includes `InvalidXmlMetadata` and
  `MetadataTooLarge { max }`. Callers that exhaustively match the error enums
  need to add the corresponding arms.
- `ResourceLimits` and `DocxTemplate::open_with_limits` configure package and
  render budgets. The default
  budgets are tuned for fairly large documents; services accepting untrusted
  input should tighten the budgets explicitly to match their own capacity.
- `RenderSession::new_subdoc_from_reader` and `new_subdoc_from_bytes` accept
  streamed and in-memory subdocuments.

## Behavioral boundaries

- The public JSON rendering entry point requires the top-level value to be an
  object; arrays, scalars, and null return a type error.
- The CLI refuses to overwrite an existing output by default. The
  direct-invocation form overwrites explicitly with `-o/--overwrite`; the
  `render` subcommand retains its documented overwrite semantics.
- Python Jinja2 Environment/extensions, arbitrary Python objects, and the DEV
  items listed in `docs/compatibility.md` are not covered by the 1.0
  compatibility promise.

## Adoption checklist

1. Pin `1.0.0` and compile against the public API described above.
2. Run your project's representative DOCX corpus and review the resource
   budgets.
3. Use wildcard arms when matching error enums, so that future new error
   categories do not cause source-level incompatibility.
4. Keep dependencies locked and run representative document tests before
   adopting later 1.x updates.
