//! Line-by-line port of `DocxTemplate.resolve_listing` (upstream
//! template.py L380-431).
//!
//! After rendering, for each `<w:p>…</w:p>` (DOTALL), then each
//! `<w:r>…</w:r>` within it, and then each `<w:t…>…</w:t>` within that,
//! substitutions are applied: take the paragraph's first `<w:pPr>…</w:pPr>`
//! and the run's first `<w:rPr>…</w:rPr>`, and expand control characters in
//! the text into OOXML:
//! `\t` -> `<w:tab/>`, `\a` -> new paragraph, `\n` -> `<w:br/>`,
//! `\f` -> page break.

use fancy_regex::Regex;
use std::sync::OnceLock;

use crate::{cap, sub};

/// A listing expansion would exceed the caller-provided output budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("resolved listing output exceeds {max} bytes")]
pub struct ListingLimitError {
    /// Maximum output size requested by the caller.
    pub max: usize,
}

fn re_paragraph() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)<w:p(?: [^>]*)?>.*?</w:p>").expect("invalid regex"))
}

fn re_run() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)<w:r(?: [^>]*)?>.*?</w:r>").expect("invalid regex"))
}

fn re_text() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)<w:t(?: [^>]*)?>.*?</w:t>").expect("invalid regex"))
}

fn re_ppr() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)<w:pPr>.*?</w:pPr>").expect("invalid regex"))
}

fn re_rpr() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?s)<w:rPr>.*?</w:rPr>").expect("invalid regex"))
}

/// Aligned with docxtpl 0.20.2 `DocxTemplate.resolve_listing`.
#[must_use]
pub fn resolve_listing(xml: &str) -> String {
    sub(re_paragraph(), xml, |para_caps| {
        let paragraph = cap(para_caps, 0);
        let paragraph_properties = re_ppr()
            .find(paragraph)
            .ok()
            .flatten()
            .map_or("", |m| &paragraph[m.start()..m.end()]);
        sub(re_run(), paragraph, |run_caps| {
            let run = cap(run_caps, 0);
            let run_properties = re_rpr()
                .find(run)
                .ok()
                .flatten()
                .map_or("", |m| &run[m.start()..m.end()]);
            sub(re_text(), run, |text_caps| {
                resolve_text(run_properties, paragraph_properties, cap(text_caps, 0))
            })
        })
    })
}

/// Bounded variant of [`resolve_listing`].
///
/// Every nested regular-expression substitution and every control-character
/// expansion checks the budget before appending, so a newline-heavy value
/// cannot allocate its fully expanded OOXML before the limit is observed.
pub fn resolve_listing_limited(xml: &str, max_bytes: usize) -> Result<String, ListingLimitError> {
    if xml.len() > max_bytes {
        return Err(ListingLimitError { max: max_bytes });
    }
    sub_limited(re_paragraph(), xml, max_bytes, |para_caps| {
        let paragraph = cap(para_caps, 0);
        let paragraph_properties = re_ppr()
            .find(paragraph)
            .ok()
            .flatten()
            .map_or("", |m| &paragraph[m.start()..m.end()]);
        sub_limited(re_run(), paragraph, max_bytes, |run_caps| {
            let run = cap(run_caps, 0);
            let run_properties = re_rpr()
                .find(run)
                .ok()
                .flatten()
                .map_or("", |m| &run[m.start()..m.end()]);
            sub_limited(re_text(), run, max_bytes, |text_caps| {
                resolve_text_limited(
                    run_properties,
                    paragraph_properties,
                    cap(text_caps, 0),
                    max_bytes,
                )
            })
        })
    })
}

fn push_limited(
    output: &mut String,
    value: &str,
    max_bytes: usize,
) -> Result<(), ListingLimitError> {
    if value.len() > max_bytes.saturating_sub(output.len()) {
        return Err(ListingLimitError { max: max_bytes });
    }
    output.push_str(value);
    Ok(())
}

fn sub_limited(
    re: &Regex,
    src: &str,
    max_bytes: usize,
    f: impl Fn(&fancy_regex::Captures<'_>) -> Result<String, ListingLimitError>,
) -> Result<String, ListingLimitError> {
    let mut output = String::with_capacity(src.len().min(max_bytes));
    let mut last = 0usize;
    for item in re.captures_iter(src) {
        let captures = match item {
            Ok(captures) => captures,
            Err(_) => break,
        };
        let Some(matched) = captures.get(0) else {
            break;
        };
        push_limited(&mut output, &src[last..matched.start()], max_bytes)?;
        push_limited(&mut output, &f(&captures)?, max_bytes)?;
        last = matched.end();
    }
    push_limited(&mut output, &src[last..], max_bytes)?;
    Ok(output)
}

fn build_replacement(parts: &[&str], max_bytes: usize) -> Result<String, ListingLimitError> {
    let mut output = String::new();
    for part in parts {
        push_limited(&mut output, part, max_bytes)?;
    }
    Ok(output)
}

fn replace_char_limited(
    src: &str,
    needle: char,
    replacement: &str,
    max_bytes: usize,
) -> Result<String, ListingLimitError> {
    let mut output = String::with_capacity(src.len().min(max_bytes));
    let mut last = 0usize;
    for (index, ch) in src.char_indices() {
        if ch != needle {
            continue;
        }
        push_limited(&mut output, &src[last..index], max_bytes)?;
        push_limited(&mut output, replacement, max_bytes)?;
        last = index + ch.len_utf8();
    }
    push_limited(&mut output, &src[last..], max_bytes)?;
    Ok(output)
}

fn resolve_text_limited(
    run_properties: &str,
    paragraph_properties: &str,
    matched: &str,
    max_bytes: usize,
) -> Result<String, ListingLimitError> {
    let xml = if matched.contains('\t') {
        let tab = build_replacement(
            &[
                "</w:t></w:r><w:r>",
                run_properties,
                "<w:tab/></w:r><w:r>",
                run_properties,
                "<w:t xml:space=\"preserve\">",
            ],
            max_bytes,
        )?;
        replace_char_limited(matched, '\t', &tab, max_bytes)?
    } else {
        matched.to_owned()
    };
    let xml = if xml.contains('\u{07}') {
        let paragraph = build_replacement(
            &[
                "</w:t></w:r></w:p><w:p>",
                paragraph_properties,
                "<w:r>",
                run_properties,
                "<w:t xml:space=\"preserve\">",
            ],
            max_bytes,
        )?;
        replace_char_limited(&xml, '\u{07}', &paragraph, max_bytes)?
    } else {
        xml
    };
    let xml = if xml.contains('\n') {
        replace_char_limited(
            &xml,
            '\n',
            "</w:t><w:br/><w:t xml:space=\"preserve\">",
            max_bytes,
        )?
    } else {
        xml
    };
    if xml.contains('\u{0c}') {
        let page = build_replacement(
            &[
                "</w:t></w:r></w:p><w:p><w:r><w:br w:type=\"page\"/></w:r></w:p><w:p>",
                paragraph_properties,
                "<w:r>",
                run_properties,
                "<w:t xml:space=\"preserve\">",
            ],
            max_bytes,
        )?;
        replace_char_limited(&xml, '\u{0c}', &page, max_bytes)
    } else {
        Ok(xml)
    }
}

/// Corresponds to the upstream internal function resolve_text: performs
/// literal replacements within a single `<w:t…>…</w:t>` match.
fn resolve_text(run_properties: &str, paragraph_properties: &str, matched: &str) -> String {
    // \t -> <w:tab/> in a new sibling run, reusing the current run's
    // properties.
    let xml = matched.replace(
        '\t',
        &format!(
            "</w:t></w:r><w:r>{rp}<w:tab/></w:r><w:r>{rp}<w:t xml:space=\"preserve\">",
            rp = run_properties
        ),
    );
    // \a -> close the current paragraph and start a new one with the current
    // pPr/rPr.
    let xml = xml.replace(
        '\u{07}',
        &format!(
            "</w:t></w:r></w:p><w:p>{pp}<w:r>{rp}<w:t xml:space=\"preserve\">",
            pp = paragraph_properties,
            rp = run_properties
        ),
    );
    // \n -> <w:br/>.
    let xml = xml.replace('\n', "</w:t><w:br/><w:t xml:space=\"preserve\">");
    // \f -> close the paragraph, insert a standalone page-break paragraph,
    // then start a new paragraph with the current pPr/rPr.
    xml.replace(
        '\u{0c}',
        &format!(
            "</w:t></w:r></w:p><w:p><w:r><w:br w:type=\"page\"/></w:r></w:p>\
             <w:p>{pp}<w:r>{rp}<w:t xml:space=\"preserve\">",
            pp = paragraph_properties,
            rp = run_properties
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::{resolve_listing, resolve_listing_limited};

    #[test]
    fn newline_becomes_br() {
        let src = r#"<w:p><w:r><w:t xml:space="preserve">V: l1
l2</w:t></w:r></w:p>"#;
        let out = resolve_listing(src);
        assert_eq!(
            out,
            r#"<w:p><w:r><w:t xml:space="preserve">V: l1</w:t><w:br/><w:t xml:space="preserve">l2</w:t></w:r></w:p>"#
        );
    }

    #[test]
    fn tab_uses_run_properties() {
        let src =
            r#"<w:p><w:r><w:rPr><w:b/></w:rPr><w:t xml:space="preserve">a	b</w:t></w:r></w:p>"#;
        let out = resolve_listing(src);
        assert!(out.contains("<w:r><w:rPr><w:b/></w:rPr><w:tab/></w:r>"));
    }

    #[test]
    fn bounded_listing_matches_and_stops_before_expansion() {
        let src = "<w:p><w:r><w:t>a\nb</w:t></w:r></w:p>";
        let expected = resolve_listing(src);
        assert_eq!(
            resolve_listing_limited(src, expected.len()).unwrap(),
            expected
        );
        assert!(resolve_listing_limited(src, expected.len() - 1).is_err());
    }
}
