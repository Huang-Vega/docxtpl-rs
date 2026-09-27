# 1.0 release line

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
