//! docxtpl-compat: upstream compatibility layer.
//!
//! Hosts special semantic rules backed by upstream evidence (docxtpl 0.20.2
//! `docxtpl/template.py`): [`patch_xml`] is a line-by-line Rust port of the
//! regex transformations in `DocxTemplate.patch_xml` (fancy-regex, with
//! lookahead/lookbehind support), including delimiter split healing,
//! cross-run merging inside tags, colspan/cellbg injection, whitespace
//! control, tr/tc/p/r structured-tag promotion, vm/hm cell merging, and
//! entity cleanup in clean_tags. No patch without upstream evidence may be
//! added.
//!
//! All transformations are pure string processing with a fixed,
//! non-configurable order; see ADR-002 for semantics.

use fancy_regex::{Captures, Regex};
use std::sync::OnceLock;

mod listing;

pub use listing::{resolve_listing, resolve_listing_limited, ListingLimitError};

/// Aligned with docxtpl 0.20.2 `DocxTemplate.patch_xml`. Pure string
/// transformations in a fixed, non-configurable order.
///
/// The 13 steps execute strictly in the order of upstream
/// `docxtpl/template.py` L84-303; each is equivalent to
/// `re.sub` with `flags=re.DOTALL` (replace all non-overlapping matches).
#[must_use]
pub fn patch_xml(src: &str) -> String {
    let src = step_delimiter_heal(src);
    let src = step_strip_tags_inside_markers(&src);
    let src = step_colspan(&src);
    let src = step_cellbg(&src);
    let src = step_ensure_space_preservation(&src);
    let src = step_split_r_tag(&src);
    let src = step_trim_left(&src);
    let src = step_trim_right(&src);
    let src = step_structured_tags(&src);
    let src = step_comment_structured_tags(&src);
    let src = step_v_merge(&src);
    let src = step_h_merge(&src);
    step_clean_tags(&src)
}

// ===========================================================================
// General substitution helpers
// ===========================================================================

/// Equivalent to Python `re.sub(pattern, repl, src, flags=re.DOTALL)`:
/// following `captures_iter`, stitches unmatched slices and callback results
/// from left to right.
///
/// fancy-regex returns `Err` when pathological backtracking exceeds its
/// budget; in that case the remaining suffix is kept verbatim and
/// substitution stops. The docxtpl corpus consists of bounded XML documents
/// that never hit this path (no semantic difference from upstream).
fn sub(re: &Regex, src: &str, f: impl Fn(&Captures) -> String) -> String {
    let mut out = String::with_capacity(src.len());
    let mut last = 0usize;
    for item in re.captures_iter(src) {
        let caps = match item {
            Ok(caps) => caps,
            Err(_) => break,
        };
        // Group 0 always exists for a valid match; if it is missing the slice
        // boundary cannot be located, so stop substituting.
        let m = match caps.get(0) {
            Some(m) => m,
            None => break,
        };
        out.push_str(&src[last..m.start()]);
        out.push_str(&f(&caps));
        last = m.end();
    }
    out.push_str(&src[last..]);
    out
}

/// Equivalent to `re.sub(pattern, repl, src, count=1)`: replace only the
/// first match; return the input unchanged when there is no match.
fn replace_first(re: &Regex, src: &str, replacement: &str) -> String {
    if let Ok(Some(caps)) = re.captures(src) {
        if let Some(m) = caps.get(0) {
            let mut out = String::with_capacity(src.len() + replacement.len());
            out.push_str(&src[..m.start()]);
            out.push_str(replacement);
            out.push_str(&src[m.end()..]);
            return out;
        }
    }
    src.to_owned()
}

/// Returns the captured string; groups that did not participate in the match
/// are treated as empty strings (the corresponding upstream groups are
/// guaranteed to participate).
fn cap<'t>(caps: &'t Captures, idx: usize) -> &'t str {
    caps.get(idx).map_or("", |m| m.as_str())
}

/// Concatenates several string slices.
fn concat(parts: &[&str]) -> String {
    let total = parts.iter().map(|s| s.len()).sum();
    let mut out = String::with_capacity(total);
    for part in parts {
        out.push_str(part);
    }
    out
}

/// Defines a regex accessor that compiles the pattern only once (a
/// compilation failure is a program bug; `expect` is allowed only here).
macro_rules! define_regex {
    ($(#[doc = $doc:literal] $name:ident = $pat:literal;)*) => {
        $(
            #[doc = $doc]
            fn $name() -> &'static Regex {
                static RE: OnceLock<Regex> = OnceLock::new();
                RE.get_or_init(|| Regex::new($pat).expect(concat!("invalid regex: ", $pat)))
            }
        )*
    };
}

define_regex! {
    #[doc = "Step 1: `(?<={)(<[^>]*>)+(?=[{#%])|(?<=[%}#])(<[^>]*>)+(?=\\})` (DOTALL)."]
    re_delimiter_heal = r"(?s)(?<=\{)(<[^>]*>)+(?=[{#%])|(?<=[%}#])(<[^>]*>)+(?=\})";

    #[doc = "Step 2 outer: starts at `{%` / `{#` / `{{` and runs up to the matching closing delimiter (DOTALL)."]
    re_tag_outer = r"(?s)\{%(?:(?!%\}).)*|{#(?:(?!#\}).)*|\{\{(?:(?!\}\}).)*";

    #[doc = "Step 2 inner: run boundaries `</w:t>…<w:t>` inside a tag (DOTALL)."]
    re_run_boundary = r"(?s)</w:t>.*?(<w:t>|<w:t [^>]*>)";

    #[doc = "Step 3 outer: the cell carrying `{% colspan expr %}` (DOTALL)."]
    re_colspan_outer = r"(?s)(<w:tc[ >](?:(?!<w:tc[ >]).)*)\{%\s*colspan\s+([^%]*)\s*%\}(.*?</w:tc>)";

    #[doc = "Steps 3/4: remove empty-text runs (DOTALL)."]
    re_empty_run = r"(?s)<w:r[ >](?:(?!<w:r[ >]).)*<w:t></w:t>.*?</w:r>";

    #[doc = "Step 3: the first `<w:gridSpan .../>` (DOTALL)."]
    re_gridspan_any = r"(?s)<w:gridSpan[^/]*/>";

    #[doc = "Step 4 outer: the cell carrying `{% cellbg expr %}` (DOTALL)."]
    re_cellbg_outer = r"(?s)(<w:tc[ >](?:(?!<w:tc[ >]).)*)\{%\s*cellbg\s+([^%]*)\s*%\}(.*?</w:tc>)";

    #[doc = "Step 4: the first `<w:shd .../>` (DOTALL)."]
    re_shd_any = r"(?s)<w:shd[^/]*/>";

    #[doc = "Steps 3/4: the `<w:tcPr ...>` opening tag (DOTALL)."]
    re_tcpr_open = r"(?s)(<w:tcPr[^>]*>)";

    #[doc = "Step 5: a bare `<w:t>` containing `{{...}}` / `{%...%}` (DOTALL)."]
    re_xml_space = r"(?s)<w:t>((?:(?!<w:t>).)*)(\{\{.*?\}\}|\{%.*?%\})";

    #[doc = "Step 6: `{{r ...}}` / `{%r ...%}` (DOTALL)."]
    re_r_split = r"(?s)(\{\{r\s.*?\}\}|\{%r\s.*?%\})";

    #[doc = "Step 7: merge `{%-` with the preceding text (DOTALL)."]
    re_trim_left = r"(?s)</w:t>(?:(?!</w:t>).)*?\{%-";

    #[doc = "Step 8: merge `-%}` with the following text (DOTALL)."]
    re_trim_right = r"(?s)-%\}(?:(?!<w:t[ >]|\{%|\{\{).)*?<w:t[^>]*?>";

    #[doc = "Step 11 outer: the cell containing `{% vm %}` (DOTALL)."]
    re_vm_outer = r"(?s)<w:tc[ >](?:(?!<w:tc[ >]).)*?\{%\s*vm\s*%\}.*?</w:tc[ >]";

    #[doc = "Step 11 inner: end of tcPr -> `<w:t>`, text before and after vm, `</w:t>` (DOTALL)."]
    re_vm_inner = r"(?s)(</w:tcPr[ >].*?<w:t(?:.*?)>)(.*?)(?:\{%\s*vm\s*%\})(.*?)(</w:t>)";

    #[doc = "Step 12 outer: the cell containing `{% hm %}` (DOTALL)."]
    re_hm_outer = r"(?s)<w:tc[ >](?:(?!<w:tc[ >]).)*?\{%\s*hm\s*%\}.*?</w:tc[ >]";

    #[doc = "Step 12 when gridSpan already exists: replace the value with the multiplication expression (DOTALL)."]
    re_hm_gridspan_num = r#"(?s)(w:gridSpan w:val=")(\d+)(")"#;

    #[doc = "Step 12: remove the `{% hm %}` tag itself (DOTALL)."]
    re_hm_tag = r"(?s)\{%\s*hm\s*%\}";

    #[doc = "Step 12 when there is no gridSpan: inner structure for inserting gridSpan (DOTALL)."]
    re_hm_inner = r"(?s)(</w:tcPr[ >].*?<w:t(?:.*?)>)(.*?)(?:\{%\s*hm\s*%\})(.*?)(</w:t>)";

    #[doc = "Step 13: inside a tag (after `{{`/`{%`, before `}}`/`%}`, DOTALL)."]
    re_clean_tags = r"(?s)(?<=\{[{%])(.*?)(?=[}%]})";
}

/// Step 9 template; `{y}` is substituted with tr/tc/p/r.
const STRUCT_PATTERN: &str =
    r"(?s)<w:{y}[ >](?:(?!<w:{y}[ >]).)*(\{%|\{\{){y} ([^}%]*(?:%\}|\}\})).*?</w:{y}>";

/// Step 10 template; `{y}` is substituted with tr/tc/p.
const COMMENT_STRUCT_PATTERN: &str =
    r"(?s)<w:{y}[ >](?:(?!<w:{y}[ >]).)*(\{#){y} ([^}#]*(?:#\})).*?</w:{y}>";

const STRUCT_TAGS: [&str; 4] = ["tr", "tc", "p", "r"];
const COMMENT_STRUCT_TAGS: [&str; 3] = ["tr", "tc", "p"];

static STRUCT_REGEXES: [OnceLock<Regex>; 4] = [const { OnceLock::new() }; 4];
static COMMENT_STRUCT_REGEXES: [OnceLock<Regex>; 3] = [const { OnceLock::new() }; 3];

/// Step 9: one compile-once regex for each tag in tr/tc/p/r order.
fn struct_regex(index: usize) -> &'static Regex {
    STRUCT_REGEXES[index].get_or_init(|| {
        let pattern = STRUCT_PATTERN.replace("{y}", STRUCT_TAGS[index]);
        Regex::new(&pattern).expect("invalid structured-tag regex")
    })
}

/// Step 10: one compile-once regex for each tag in tr/tc/p order.
fn comment_struct_regex(index: usize) -> &'static Regex {
    COMMENT_STRUCT_REGEXES[index].get_or_init(|| {
        let pattern = COMMENT_STRUCT_PATTERN.replace("{y}", COMMENT_STRUCT_TAGS[index]);
        Regex::new(&pattern).expect("invalid comment-structured-tag regex")
    })
}

// ===========================================================================
// The 13 steps, each corresponding to upstream template.py
// ===========================================================================

/// Step 1 (upstream L89-95): delimiter healing.
///
/// Removes XML markup sandwiched between `{` and `{`/`%`/`#`, and between
/// `%`/`}`/`#` and `}`, so that `{{`/`}}`/`{%`/`%}`/`{#`/`#}` split by Word
/// across different runs are healed back together.
fn step_delimiter_heal(src: &str) -> String {
    sub(re_delimiter_heal(), src, |_| String::new())
}

/// Step 2 (upstream L97-110): cross-run merging inside tags.
///
/// For every tag match starting at `{%`/`{#`/`{{`, removes all
/// `</w:t>…<w:t>` boundaries inside it (including attributed `<w:t ...>`).
fn step_strip_tags_inside_markers(src: &str) -> String {
    sub(re_tag_outer(), src, |caps| {
        let whole = cap(caps, 0);
        sub(re_run_boundary(), whole, |_| String::new())
    })
}

/// Step 3 (upstream L112-133): `{% colspan expr %}`.
///
/// The carrying cell = g1 + g3 (the tag itself is discarded): first remove
/// empty-text runs, then remove the first `<w:gridSpan .../>`, and then
/// inject after every `<w:tcPr ...>`
/// `<w:gridSpan w:val="{{expr}}"/>` (expr is the verbatim g2).
fn step_colspan(src: &str) -> String {
    sub(re_colspan_outer(), src, |caps| {
        let cell = concat(&[cap(caps, 1), cap(caps, 3)]);
        let cell = sub(re_empty_run(), &cell, |_| String::new());
        let cell = replace_first(re_gridspan_any(), &cell, "");
        let injection = concat(&["<w:gridSpan w:val=\"{{", cap(caps, 2), "}}\"/>"]);
        sub(re_tcpr_open(), &cell, |c| concat(&[cap(c, 1), &injection]))
    })
}

/// Step 4 (upstream L135-156): `{% cellbg expr %}`.
///
/// Same as step 3: remove empty-text runs, remove the first `<w:shd .../>`,
/// and inject after `<w:tcPr ...>`
/// `<w:shd w:val="clear" w:color="auto" w:fill="{{expr}}"/>`.
fn step_cellbg(src: &str) -> String {
    sub(re_cellbg_outer(), src, |caps| {
        let cell = concat(&[cap(caps, 1), cap(caps, 3)]);
        let cell = sub(re_empty_run(), &cell, |_| String::new());
        let cell = replace_first(re_shd_any(), &cell, "");
        let injection = concat(&[
            "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{{",
            cap(caps, 2),
            "}}\"/>",
        ]);
        sub(re_tcpr_open(), &cell, |c| concat(&[cap(c, 1), &injection]))
    })
}

/// Step 5 (upstream L158-164): add `xml:space="preserve"` to a bare `<w:t>`
/// that contains tags.
///
/// Matches only `<w:t>` without any attributes; replaces it with
/// `<w:t xml:space="preserve">` + g1 + g2; the text after the tag is
/// preserved.
fn step_ensure_space_preservation(src: &str) -> String {
    sub(re_xml_space(), src, |caps| {
        concat(&["<w:t xml:space=\"preserve\">", cap(caps, 1), cap(caps, 2)])
    })
}

/// Step 6 (upstream L165-170): `{{r …}}` / `{%r …%}` get their own run.
///
/// Closes the current run before the tag, carries the tag in a new
/// unformatted run, and finally opens another unformatted run.
fn step_split_r_tag(src: &str) -> String {
    sub(re_r_split(), src, |caps| {
        let tag = cap(caps, 1);
        concat(&[
            "</w:t></w:r><w:r><w:t xml:space=\"preserve\">",
            tag,
            "</w:t></w:r><w:r><w:t xml:space=\"preserve\">",
        ])
    })
}

/// Step 7 (upstream L172-173): merge `{%-` with the preceding text.
///
/// The span from after `</w:t>` up to `{%-` (which must not cross another
/// `</w:t>`) is replaced wholesale with `{%`.
fn step_trim_left(src: &str) -> String {
    sub(re_trim_left(), src, |_| "{%".to_owned())
}

/// Step 8 (upstream L174-177): merge `-%}` with the following text.
///
/// Between `-%}` and the next `<w:t...>` there must be no
/// `<w:t `/`<w:t>`/`{%`/`{{`; the whole span is replaced with `%}`.
fn step_trim_right(src: &str) -> String {
    sub(re_trim_right(), src, |_| "%}".to_owned())
}

/// Step 9 (upstream L179-188): structured-tag promotion, executed once each
/// in strict tr -> tc -> p -> r order.
///
/// Replaces the whole carrying element `<w:y …>…{%y …%}…</w:y>` (or
/// `{{y …}}`) with g1 (`{%` or `{{`) + a single space + g2 (the tag body
/// including the closing delimiter).
fn step_structured_tags(src: &str) -> String {
    let mut src = src.to_owned();
    for index in 0..STRUCT_TAGS.len() {
        src = sub(struct_regex(index), &src, |caps| {
            concat(&[cap(caps, 1), " ", cap(caps, 2)])
        });
    }
    src
}

/// Step 10 (upstream L190-197): comment structured-tag promotion, in
/// tr -> tc -> p order (excluding r).
///
/// Same as step 9, but applies to `{#y …#}`.
fn step_comment_structured_tags(src: &str) -> String {
    let mut src = src.to_owned();
    for index in 0..COMMENT_STRUCT_TAGS.len() {
        src = sub(comment_struct_regex(index), &src, |caps| {
            concat(&[cap(caps, 1), " ", cap(caps, 2)])
        });
    }
    src
}

/// Step 11 (upstream L199-228): `{% vm %}` vertical merge.
///
/// Inserts between `</w:tcPr>` and `<w:t>` inside the cell
/// `<w:vMerge w:val="{% if loop.first %}restart{% else %}continue{% endif %}"/>`,
/// and wraps the text on both sides of the vm tag (g2+g3) with
/// `{% if loop.first %}…{% endif %}`; `</w:t>` (g4) is always preserved.
/// When the inner regex does not match, the whole cell is returned
/// unchanged.
fn step_v_merge(src: &str) -> String {
    sub(re_vm_outer(), src, |caps| {
        let cell = cap(caps, 0);
        sub(re_vm_inner(), cell, |m| {
            concat(&[
                "<w:vMerge w:val=\"{% if loop.first %}restart{% else %}continue{% endif %}\"/>",
                cap(m, 1),
                "{% if loop.first %}",
                cap(m, 2),
                cap(m, 3),
                "{% endif %}",
                cap(m, 4),
            ])
        })
    })
}

/// Step 12 (upstream L230-287): `{% hm %}` horizontal merge.
///
/// - If the cell already contains `w:gridSpan`: replace the value with
///   `{{ N * loop.length }}` and remove the hm tag;
/// - otherwise: insert between `</w:tcPr>` and `<w:t>`
///   `<w:gridSpan w:val="{{ loop.length }}"/>`, preserving g1-g4;
///
/// The return value of both branches is wrapped wholesale in
/// `{% if loop.first %}…{% endif %}`.
fn step_h_merge(src: &str) -> String {
    sub(re_hm_outer(), src, |caps| {
        let cell = cap(caps, 0);
        let patched = if cell.contains("w:gridSpan") {
            let multiplied = sub(re_hm_gridspan_num(), cell, |m| {
                concat(&[cap(m, 1), "{{ ", cap(m, 2), " * loop.length }}", cap(m, 3)])
            });
            sub(re_hm_tag(), &multiplied, |_| String::new())
        } else {
            sub(re_hm_inner(), cell, |m| {
                concat(&[
                    "<w:gridSpan w:val=\"{{ loop.length }}\"/>",
                    cap(m, 1),
                    cap(m, 2),
                    cap(m, 3),
                    cap(m, 4),
                ])
            })
        };
        concat(&["{% if loop.first %}", &patched, "{% endif %}"])
    })
}

/// Step 13 (upstream L289-301): clean_tags.
///
/// Acts only inside tags, after `{{`/`{%` and before `}}`/`%}`; performs
/// literal replacements in the fixed upstream order:
/// `&#8216;`->`'`, `&lt;`->`<`, `&gt;`->`>`,
/// left/right double quotation marks (U+201C/U+201D) -> `"`, left/right
/// single quotation marks (U+2018/U+2019) -> `'`.
fn step_clean_tags(src: &str) -> String {
    sub(re_clean_tags(), src, |caps| {
        cap(caps, 0)
            .replace("&#8216;", "'")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace(['\u{201C}', '\u{201D}'], "\"")
            .replace(['\u{2018}', '\u{2019}'], "'")
    })
}
