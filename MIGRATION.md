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
- use `for_each_editable_story_with_resources` with `insert_inline_image` when
  Story XML, media, and relationships must share one rollback boundary.

The old `StoryScope`, `StoryKind`, `CancellationToken`, `CancellationError`, and
`ResourceLimits` types remain available. In particular, 1.3 does not add
variants to the exhaustively matchable 1.2 story enums.

### Resource-aware inline images

The resource-aware Story context can insert a registered image directly into
an existing `w:r`. The image relationship is owned by the current Story, exact
media and relationships are reused, and all changes roll back together:

```rust
use std::sync::Arc;
use docxtpl_rs::{
    EditableStorySelection, FailurePolicy, ImageLayout, InlineImageOptions,
};

rendered.postprocess(|pipeline| {
    pipeline.pass("insert-photo", FailurePolicy::Abort, |transaction| {
        transaction.for_each_editable_story_with_resources(
            EditableStorySelection::BODY,
            |context| {
                let media = context.resources().register_media_bytes(
                    "photo.png",
                    Arc::clone(&photo_bytes),
                )?;
                let run = context.story().document()
                    .descendants(context.story().document().root())
                    .into_iter()
                    .find(|node| context.story().document().tag(*node).is_some_and(|tag| {
                        tag.ns == docxtpl_xml::ns_uri::W && tag.local == "r"
                    }))
                    .expect("target run");
                let inserted = context.insert_inline_image(
                    run,
                    &media,
                    &InlineImageOptions {
                        layout: ImageLayout::FitWithin {
                            width: 2_000_000,
                            height: 1_000_000,
                        },
                        description: Some("Product photo".into()),
                        ..InlineImageOptions::default()
                    },
                )?;
                context.clone_drawing_to_run(inserted.drawing, run)?;
                Ok(())
            },
        )?;
        Ok(())
    })?;
    Ok(())
})?;
# Ok::<(), docxtpl_rs::Error>(())
```

`clone_drawing_to_run` retains the source image/link relationship ids but
allocates fresh `wp:docPr` and `pic:cNvPr` ids. Before serialization, newly
inserted or cloned drawings are checked for duplicate ids and dangling image or
hyperlink relationships.

### WordML fragment import

`WordFragment` imports selected top-level paragraphs and tables from another
DOCX package without requiring callers to copy relationships manually:

```rust
use docxtpl_rs::{FragmentImportOptions, Package, PackageLimits, WordFragment};
use std::io::Cursor;

let source = Package::from_reader(Cursor::new(source_docx), &PackageLimits::default())?;
let mut fragment = Some(WordFragment::from_package(source, "word/document.xml")?);

transaction.for_each_editable_story_with_resources(
    EditableStorySelection::BODY,
    |context| {
        let target = context.story().document().root();
        context.import_fragment(
            target,
            fragment.take().expect("body is visited once"),
            FragmentImportOptions {
                placement: docxtpl_rs::FragmentPlacement::Append,
            },
        )?;
        Ok(())
    },
)?;
# Ok::<(), docxtpl_rs::Error>(())
```

The import owns its source package and is consumed by one import operation.
Embedded image bytes are registered through the same media catalog, external
hyperlinks are recreated for the destination Story owner, internal anchors and
bookmarks are renamed together, numbering definitions are copied with fresh
ids, and Drawing ids are allocated from the destination Story. All target
changes share the surrounding pass rollback boundary.

The first-stage importer deliberately rejects chart, OLE, SmartArt, VML image,
note/comment references, picture numbering, and unrecognized relationship
attributes. These cases return `Error::UnsupportedFragmentFeature` instead of
silently leaving a dangling relationship.

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

For in-memory uploads, use the symmetric byte-backed entry point. The name is
only a hint: the actual bytes determine the extension and Content Type. The
supplied immutable allocation is shared with the package rather than copied:

```rust
use std::sync::Arc;

let upload: Arc<[u8]> = Arc::from(image_bytes);
let media = transaction.register_media_bytes("upload.bin", upload)?;
let rid = transaction.relate_image("word/document.xml", &media)?;
```

Code that already owns a verified file snapshot can instead call
`register_media_source(name, MediaSource::File(source))`. File media remains
file-backed and is streamed during ZIP output. Registration verifies and probes
the snapshot incrementally without retaining the complete file, and final
output verifies it again.

Custom passes can return stable operational classifications through the
structured pipeline without wrapping business validation failures as I/O
errors:

```rust
rendered.postprocess_structured(|pipeline| {
    pipeline.pass_structured("fragment", FailurePolicy::Abort, |_transaction| {
        Err(PostprocessError::custom(
    "fragment.image_placeholder_missing",
    "fragment image has no registered source",
)
.with_part("word/document.xml")
.into())
    })?;
    Ok(())
})?;
```

With `FailurePolicy::WarnAndRollback`, the code and safe message are copied to
the pass warning; `rolled_back` and `touched_parts` continue to describe the
transaction outcome.

When Story editing also needs media or relationships, use the resource-aware
single-pass editor. Relationship ownership is derived from the current Story,
so the same closure works for body, headers, footers, notes, and comments:

```rust
transaction.for_each_editable_story_with_resources(
    EditableStorySelection::ALL,
    |context| {
        let media = context.resources().register_media_path("photo.png")?;
        let rid = context.resources().relate_image(&media)?;
        let story = context.story_mut();
        // Insert or update DrawingML in story.document_mut() using rid.
        let _ = rid;
        Ok(())
    },
)?;
```

The Story is parsed and serialized at most once. Its XML, registered media,
Content Types, and owner relationships roll back together if the pass fails.
Media registration in later passes reuses the same document-local catalog;
`pipeline.media_catalog_metrics()` reports scanned parts, actually hashed
bytes, catalog hits, and reused media.

If upload normalization already needs image metadata, probe a file once and
reuse the library-created handle:

```rust
let probed = ProbedMediaFile::open("photo.png", &ResourceLimits::default())?;
let media = transaction.register_probed_media(&probed)?;
```

The handle cannot accept a caller-supplied digest. It enforces the configured
single-entry limit, keeps the registered package part file-backed, and final
ZIP output rejects replacement, truncation, or modification of the source.

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

For long multi-Story passes, automatic eviction can be enabled explicitly:

```rust
pipeline.set_part_cache_policy(PartCachePolicy::EvictAbove {
    resident_bytes: 128 * 1024 * 1024,
});
```

`Retain` remains the default. A high-water check runs after media-catalog
construction and after each visited Story, releasing only clean lazy buffers
backed by the reopenable source ZIP.
Dirty/new parts, file-backed media, and transaction rollback snapshots are
never evicted. Use `RenderedDocument::postprocess_with_metrics` when extended
observability is needed. Its `DetailedPassReport::resources` reports eviction
runs and released bytes, while `residency_before` and `residency_after` expose
the pass boundary.

The `for_each_*_with_metrics` Story APIs return `DetailedStoryEditReport`, whose
`metrics` field contains parsed/serialized byte totals and elapsed times.
`DetailedPassReport::transaction` exposes snapshot/add counts and byte sizes;
its `resources` field also distinguishes added/reused media and relationships
and records `peak_resident_bytes` at mutation, media-catalog, and Story
checkpoints. Recoverably rolled-back passes report `rollback_elapsed`.
`Package::save_with_atomic_report` returns `AtomicSaveReport`, including
`temporary_file_bytes`, `temporary_sync_elapsed`, and `atomic_replace_elapsed`
alongside the stable `PackageWriteReport`. These separate detailed report types
preserve source compatibility for callers that construct the original public
reports. The metrics are counters only and do not retain part names beyond the
existing `touched_parts`, URLs, source paths, or application data.

Controlled media probing hashes byte and file inputs in 64 KiB chunks, and OPC
validation checks cancellation between parts and relationships. Controlled
atomic saves therefore stop before destination replacement when cancellation
or a deadline is observed during either phase.

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
