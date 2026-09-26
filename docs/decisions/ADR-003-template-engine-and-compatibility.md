# ADR-003: Template Engine Selection and MiniJinja/Jinja2 Compatibility Strategy

- Status: Accepted (P0)
- Decision date: 2026-09-24

## Decision

1. The rendering engine is **MiniJinja 2.x**: its syntax, filters, and whitespace control are highly aligned with Jinja2; it is pure Rust with no Python dependency. The `preserve_order`, `unicode`, `json`, and `urlencode` features are enabled, and `serde_json` enables both `preserve_order` and `float_roundtrip`, so that context/object keys are not reordered and JSON float parsing does not lose the bit pattern.
2. Defaults aligned with upstream:
   - `autoescape = false` (upstream default of `render(context, jinja_env=None, autoescape=False)`);
   - undefined values use lenient behavior (rendered as an empty string, treated as false by `{% if %}`), aligned with Jinja2's default `Undefined`;
   - whitespace control `trim_blocks=false / lstrip_blocks=false`, with `{%- -%}` semantics identical to Jinja2.
3. Upstream inserts `\n` before every `<w:p[ >]` before rendering and removes it afterwards, solely for line-number location and unrelated to the diff; the same is implemented here.
4. Declared syntax subset: Unicode identifiers, variables/expressions, if/for/set, comments
   `{# #}`, whitespace control, the literal `{_{ }_}` escape, and the common
   built-in filters/tests already covered by MiniJinja (upper/lower/join/length/default/replace/trim/map/
   selectattr/sort/sum, etc.); no compatibility promise is made for macros.
5. A compatibility override layer is provided for known output differences between MiniJinja and Jinja2 3.1.6:
   - `tojson` uses Jinja2's default key sorting, separator spaces, `ensure_ascii`, float spelling,
     HTML-safe character replacement, and `indent`/type-error semantics;
   - `urlencode` distinguishes string path encoding from query encoding of mappings/key-value sequences; the latter
     uses `+` for spaces and encodes `/`;
   - `escape`/`e`, `forceescape`, the global autoescape formatter, and `xmlattr`
     use MarkupSafe-style escapes such as `&#34;`/`&#39;` and preserve safe-value semantics;
   - Plain final output as well as `string`, `join` (including `attribute`), `replace`, and the common
     positional `%s` use Python's container/boolean/None/float spelling; `join`/`replace`
     also preserve MarkupSafe safe-value propagation and enforce the 64 MiB cap at the internal concatenation stage;
   - Ordinary iteration over JSON/context objects preserves input insertion order; the key sorting of `tojson`
     is that filter's own Jinja2 semantics and does not conflict with preserve-order.
6. Not supported: Jinja2 extensions, line statements, arbitrary Python objects/callables,
   and custom `jinja_env` injection. Each explicitly compatible filter/test is recorded in
   `docs/compatibility.md`; there is no blanket claim of "fully compatible with Jinja2".
7. Engine boundaries: MiniJinja's `~` operator has no environment-level stringification hook; nor does the value layer
   retain the original Python types of tuple/range/bytes. Rather than guessing semantics through template-source rewriting,
   these spellings, along with printf mapping and `%r/%a`, remain registered as DEV-0015.

## Validation

Frozen oracle fixtures cover filters, defaults, undefined behavior, whitespace control, and the loop variable
`loop`. Subsequent compatibility-hardening unit tests additionally cover Unicode identifiers/casing, object insertion
order, common filters and tests, `tojson` with nested objects/indentation/floats/error inputs, Python-style
final output and the common `string`/`join`/`replace`/`format` paths, string and
mapping/iterable `urlencode`, autoescape, `escape`/`forceescape`, and
`xmlattr`. These regressions serve to narrow known differences and do not expand into a commitment to the entire
Jinja2 plugin ecosystem.

## Post-Freeze Addendum (2026-09-26)

- `serde_json` subsequently enables `arbitrary_precision`; JSON integers are preserved with exact fidelity within the
  signed i128 range, and out-of-range values explicitly return `InvalidArgument` (DEV-0016).
- The common Python methods on JSON dict/list/string, as well as the deterministic
  filters/globals/tests and macro/call/namespace/filter blocks listed in the compatibility matrix, are covered by unit
  tests and table-driven live probes against Jinja2 3.1.6. RichText-like values also switch to a wrapper that preserves
  Python-object truthiness and `__html__` identity; the transform-filter boundaries for InlineImage remain
  documented in DEV-0017.
- Promises about the aforementioned `striptags`, type tests, and callables are made only for the paths explicitly
  listed in the compatibility matrix and live probe tables. The MiniJinja VM/type model may still differ on
  modulo with a negative divisor, `bool is number`, `is sequence` for strings/
  mappings, heterogeneous comparisons, and the `True`/`1` key identity of generic mappings; these are registered as
  DEV-0018.
- `striptags` currently decodes only a limited set of named entities; the default environment does not register
  `urlize`, `random`, `lipsum`, or `randrange`, and provides no template loader, so
  `include`/`import`/`extends` are not promised. These library-surface boundaries are registered as DEV-0019.
- `RenderOptions::with_environment_configurator` affects actual rendering only; static template
  introspection still uses the fixed environment and does not apply the configurator (DEV-0013).
- The rendering pipeline applies a single-buffer 64 MiB budget to allocation-time XML serialization, layered listing
  expansion, image replacement, and expansion-type filter output, with MiniJinja fuel set to
  10 000 000; intermediate collections such as `split`/`urlencode`/`join` are additionally subject to a 524 288-item
  budget. These are per-path budgets rather than a process RSS cap, and belong to the DEV-0001 security hardening.
