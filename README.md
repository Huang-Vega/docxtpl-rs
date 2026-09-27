# docxtpl-rs

A Rust library and command-line tool for rendering Jinja templates in the DOCX
body, headers/footers, core properties, and footnotes.
This is an independent Rust implementation. It is not an official port and is
not affiliated with or endorsed by the Python docxtpl project. Python docxtpl
is used only as the pinned development-time compatibility oracle; it is not a
runtime dependency of any published Rust crate.
The current version is `1.0.0`, aligned with the P0–P7 frozen feature set
of Python docxtpl 0.20.2. The 1.0 line is the first public compatibility
baseline; earlier development
snapshots are not supported release or migration targets. See the
[1.0 release-line guide](https://github.com/Huang-Vega/docxtpl-rs/blob/master/MIGRATION.md)
for the public API baseline and adoption checklist.

## Usage

Rust 1.85 or newer is required. After cloning the repository, run:

```sh
cargo build -p docxtpl-cli
cargo run -p docxtpl-cli -- render template.docx context.json output.docx
```

Use the exact stable version:

```sh
cargo add docxtpl-rs@1.0.0
cargo install docxtpl-cli --version 1.0.0 --locked
```

The release procedure and crate order are described in
[RELEASING.md](https://github.com/Huang-Vega/docxtpl-rs/blob/master/RELEASING.md).
Release evidence is tracked in
[docs/release-readiness.md](https://github.com/Huang-Vega/docxtpl-rs/blob/master/docs/release-readiness.md).

`context.json` is a JSON object, for example
`{"name":"Vega","items":[{"name":"Apple"}]}`.
The CLI enforces a 64 MiB limit on the context file and errors out before JSON
parsing if it is exceeded.
A direct-invocation form compatible with Python docxtpl 0.20.2 is also
available:

```sh
docxtpl template.docx context.json output.docx
docxtpl -o -q template.docx context.json output.docx
```

The direct invocation validates the `.docx`/`.json` extensions and the input
files; if the output already exists, it errors out explicitly by default rather
than waiting for input in a non-interactive environment — use
`-o`/`--overwrite` to overwrite explicitly.
As with the Python CLI, extensions are case-sensitive. The success message of
the direct invocation is written to stdout;
`-q`/`--quiet` suppresses only that message. To keep existing scripts
compatible, the `render` subcommand keeps its original overwrite behavior, path
handling, and stderr success message, and continues to support `--autoescape`.

For library usage, see the
[docxtpl-rs examples](https://github.com/Huang-Vega/docxtpl-rs/blob/master/crates/docxtpl-rs/src/lib.rs).

## Supported scope

Plain variables, control structures, advanced tables,
RichText/Listing/InlineImage, string rendering in headers/footers and
footnotes, external Subdoc merging, media/embedded replacement, and template
variable introspection.
For exact use cases, known deviations, and oracle results, see the
[compatibility list](https://github.com/Huang-Vega/docxtpl-rs/blob/master/docs/compatibility.md).

Python Jinja2 Environment/extensions, arbitrary Python objects, and other edge
cases are all listed under `unsupported` / `DEV-*` in the compatibility list;
Rust callers can register MiniJinja-native filters/tests/functions via
`RenderOptions::with_environment_configurator` (these affect rendering only and
do not participate in static template introspection).
Default input and rendering budgets are described in
[Resource limits](https://github.com/Huang-Vega/docxtpl-rs/blob/master/docs/security-limits.md).
The default document capacity is 600 MiB and the ZIP entry limit is 6000; file
templates are reopened by path, ordinary ZIP parts are decompressed on demand,
unmodified entries are raw-copied on write, and the output is streamed directly
to a temporary file in the same directory. Library callers can use
`DocxTemplate::open_with_limits(..., ResourceLimits::default())` to adjust
package size, rendered XML, and template fuel; the compression-ratio protection
stays enabled by default.

## Verification

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
python -m pip install -r tests/oracle/requirements.txt
python tests/oracle/runner.py
cargo test -p docxtpl-rs --features oracle
DOCXTPL_PYTHON_ORACLE=python cargo test -p docxtpl-template live_python_oracle_for_new_compat_surface
cargo build -p docxtpl-cli
python tests/office_check.py
```

The last step requires LibreOffice to be installed.

P8 additionally requires packaged-crate and clean-install smoke tests:

```sh
python tests/license_audit.py
cargo package --workspace --locked --no-verify
python tests/release_smoke.py
```
