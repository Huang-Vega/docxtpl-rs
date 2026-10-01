# Release process

This project's registry packages consist of seven crates. A stable release must
use a clean working tree, an immutable commit, and a passing release-candidate
workflow; you must not publish directly from a development directory while
skipping the gates.

## RC gates

1. Confirm that `Cargo.toml`, `Cargo.lock`, README, CHANGELOG, and the migration
   guide all agree on the version.
2. Run formatting, Clippy, workspace tests, the oracle, MSRV, the license audit,
   and the documentation build.
3. Run `cargo package --workspace --locked --no-verify`, then
   `python tests/release_smoke.py`. This script uses only the generated
   `.crate` files: in an isolated directory it builds the library example,
   installs the CLI, renders a DOCX, and inspects the output package.
   Inspect `cargo package --list -p <name> --locked` for all seven crates. No
   package may contain Python source, oracle code, upstream fixture templates,
   a virtual environment, or repository-only test assets; Python docxtpl is a
   development-time compatibility oracle, not a runtime or packaged component.
4. No unresolved failures across the three platforms, LibreOffice, the
   representative Microsoft Word samples, and the periodic fuzz runs.
5. The RC tag must correspond to the workspace version, for example
   `v1.0.0`.

## Registry publish order

Internal path dependencies become exact registry versions when packaged, so
publish in dependency-topological order and wait for each crate to become
visible in the index before continuing:

1. `docxtpl-compat`, `docxtpl-opc`, `docxtpl-rich`, `docxtpl-xml`
2. `docxtpl-template`
3. `docxtpl-rs`
4. `docxtpl-cli`

Before publishing, run `cargo publish -p <name> --dry-run --locked` for each
crate in turn; the real `cargo publish`, Git tags, and GitHub Release must be
performed explicitly by a maintainer. For the final `1.0.0`, all
workspace/internal dependency versions must be bumped together from RC to
`1.0.0`.

## Rollback

crates.io versions cannot be overwritten. If a problem is found, stop
publishing further crates, yank the problematic versions, and publish a new RC
after fixing the issue; pushed tags must not be moved. The release log must
record which versions were yanked and why.
