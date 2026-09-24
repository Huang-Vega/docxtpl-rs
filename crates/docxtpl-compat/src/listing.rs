//! `DocxTemplate.resolve_listing` 的逐条移植（上游 template.py L380–431）。
//!
//! 渲染后对每个 `<w:p>…</w:p>`（DOTALL），依次对其中的 `<w:r>…</w:r>`、
//! 再对其中的 `<w:t…>…</w:t>` 做替换：取段落首个 `<w:pPr>…</w:pPr>` 与
//! run 首个 `<w:rPr>…</w:rPr>`，把文本中的控制符展开为 OOXML：
//! `\t`→`<w:tab/>`，`\a`→换段，`\n`→`<w:br/>`，`\f`→分页符。

use fancy_regex::Regex;
use std::sync::OnceLock;

use crate::{cap, sub};

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

/// 对齐 docxtpl 0.20.2 `DocxTemplate.resolve_listing`。
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

/// 对应上游内部函数 resolve_text：在单个 `<w:t…>…</w:t>` 匹配内做字面替换。
fn resolve_text(run_properties: &str, paragraph_properties: &str, matched: &str) -> String {
    // \t → 同级新 run 中的 <w:tab/>，run 属性沿用当前 run。
    let xml = matched.replace(
        '\t',
        &format!(
            "</w:t></w:r><w:r>{rp}<w:tab/></w:r><w:r>{rp}<w:t xml:space=\"preserve\">",
            rp = run_properties
        ),
    );
    // \a → 关闭当前段落，以当前 pPr/rPr 另起一段。
    let xml = xml.replace(
        '\u{07}',
        &format!(
            "</w:t></w:r></w:p><w:p>{pp}<w:r>{rp}<w:t xml:space=\"preserve\">",
            pp = paragraph_properties,
            rp = run_properties
        ),
    );
    // \n → <w:br/>。
    let xml = xml.replace('\n', "</w:t><w:br/><w:t xml:space=\"preserve\">");
    // \f → 关闭段落，插入独立分页符段落，再以当前 pPr/rPr 另起一段。
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
    use super::resolve_listing;

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
}
