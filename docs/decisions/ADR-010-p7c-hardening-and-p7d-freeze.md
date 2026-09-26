# ADR-010: P7c Compatibility Hardening and P7d 0.8.x Freeze

Date: 2026-09-26
Status: Accepted
Phase: P7c (fuzz/property tests, limits, performance, cross-platform) and P7d (compatibility report/0.8.x)

## Context

P7a/P7b and subsequent gap-fixing had converged public functionality and the real Word corpus to 102 render fixtures
fully green at byte equality, but P7 acceptance also requires no panic on malformed input, a resource-limit review,
performance regressions, and cross-platform gates. These items do not change docxtpl compatibility semantics, so no new
oracle fixtures are added.

## Decision

1. Add proptest coverage for three high-risk pure entry points: OPC random/truncated bytes, XML strict/recovery with
   arbitrary UTF-8 and XML-like input, and `patch_xml` with arbitrary and marker-heavy input. The case counts are fixed
   at 256/512 so that every workspace test run is reproducible and time-bounded.
2. `tests/p7c_audit.py` scans the ZIP central directory of the frozen templates and records peaks; the release internal
   benchmark runs 15 iterations each for basic variables, composite tables, and real Word dynamic tables, separately
   recording open/render/write timing, output size, and peak RSS. A render regression beyond 20% under same-platform
   `--compare` returns failure.
3. Performance baselines are machine-dependent evidence and are not hard-compared across platforms; local `--compare`
   is only suitable for same-machine retests. Cross-platform correctness is borne by Windows, Linux, and macOS CI, with
   the Python oracle and LibreOffice pinned to the Linux job; configuration must not be written up as actually passing
   before CI has run.
4. The P7 public scope is governed by the compatible entries in `docs/compatibility.md` and DEV-0001–0014. Of the 102
   render fixtures, 98 are byte-exact MATCH and 4 have matching error categories, i.e. 100% of the declared scope and
   100% of blocking cases, above the 95% threshold.
5. The workspace release candidate is 0.8.0; representative real-machine Office spot checks are covered by LibreOffice
   and Microsoft Word, and a real macOS run has been added, though long-term fuzz evidence is still required before
   release. Peak RSS collection supports Windows/Linux/macOS. Publishing to a registry and creating a Git tag are
   external release actions and are outside this ADR.

## Measured Results

- Corpus: 127 templates; largest archive 38 750 B with 23 entries, largest single entry 438 131 B, total decompressed
  833 014 B, compression ratio 32.156 — all well below the default limits.
- Windows AMD64 / Python 3.13.13 release staged baselines (15 iterations each):
  `r2_var_basic` render 15.699 ms, `r3_combo_invoice` 17.799 ms,
  `p7b_dynamic_table` 27.610 ms; see `docs/p7c-performance-baseline.json` for details.
- Ubuntu 24.04 x86_64 / rustc 1.98.1 / Python 3.12.3 measured workspace, clippy,
  fmt, and oracle all green; LibreOffice 24.2.7.2 opened and re-saved 8/8. The Linux release staged render baselines
  are 8.409 / 9.869 / 20.747 ms in order; see `docs/p7c-performance-linux.json` for details.
- Windows 11 Pro x64 / Microsoft Word 16.0.17932.20700 x64 opened, re-saved, and reopened 10 representative
  Rust outputs, 10/10 passed; the 11 pages exported by Word passed 11/11 manual visual checks, and the re-saved DOCX
  ZIP/XML structure checks passed 10/10. Details are in `docs/p7d-word-smoke.json`.
- The 6 newly added property tests all pass; the complete gates and oracle results are recorded in
  `docs/p7-compatibility-report.md`.

- macOS 26.6.2 arm64 / rustc 1.98.1 / Python 3.14.4 passed all gates,
  and LibreOfficeDev 26.8.0.0.alpha0 opened and re-saved 8/8; fixed a Unicode-slicing panic in the strict `<!` branch
  discovered by property tests. See `docs/p7d-macos-verification.md` and
  `docs/p7c-performance-macos.json` for details.

## Impact

Library runtime behavior is unchanged. The newly added test dependency `proptest` is dev-dependency only; the
performance scripts build only under the project `target/` and create temporary output. Subsequent P8 may reuse
`--compare` to check release candidates.

## Post-Freeze Addendum (2026-09-26)

The "DEV-0001–0014" and "library runtime behavior is unchanged" statements in this ADR describe only the P7c/P7d freeze
actions, not the total scope of the current working tree. After the freeze, the Jinja/value compatibility layer,
Rust-native environment configuration, signed-i128 boundaries, randomized image token namespaces, XmlPart UTF-16/32
decoding, Subdoc reader/bytes, CLI direct mode, and forward master accessibility extensions were added.

These subsequent items use standalone unit tests, dynamic DOCX regressions, and live Jinja2 probes, and do not
retroactively rewrite the 102 render / 324 total denominators. The current deviation register is governed by
`docs/compatibility.md`, now reaching DEV-0019; DEV-0005/0009/0012 are closed and retain their numbering holes.
