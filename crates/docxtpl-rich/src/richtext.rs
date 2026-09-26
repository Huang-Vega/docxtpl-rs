//! RichText / RichTextParagraph: byte-level alignment with docxtpl 0.20.2 `richtext.py`.
//!
//! - Run template: `<w:r>` + (`<w:rPr>…</w:rPr>` when properties are non-empty) +
//!   `<w:t xml:space="preserve">…</w:t></w:r>`;
//! - Properties are concatenated in the fixed order of upstream `add()` (rStyle → color → shd
//!   → sz/szCs → vertAlign → b/bCs → i/iCs → u → strike → rFonts → rtl → lang);
//! - Empty-string semantics (verified by probes): the constructor skips it via the `if text:`
//!   falsy check; `add()` does **not** skip it (it emits an empty `<w:t>`; upstream has that
//!   check commented out).

/// Escaping byte-for-byte identical to Python `html.escape(text)` (quote defaults to True).
///
/// Note: upstream `richtext.py` / `listing.py` call `escape(text)` without `quote=False`, so
/// all five characters are escaped. Probe-verified:
/// `RichText('a"b\'c&<>z').xml` outputs `a&quot;b&#x27;c&amp;&lt;&gt;z`.
pub(crate) fn escape_html(text: &str) -> String {
    // Same order as CPython html.escape: replace & first so its replacement is not escaped
    // again
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// Set of valid values for `w:u w:val` (the hard-coded list inside upstream add()).
const VALID_UNDERLINES: &[&str] = &[
    "single",
    "double",
    "thick",
    "dotted",
    "dash",
    "dotDash",
    "dotDotDash",
    "wave",
];

/// Rich-text properties of a single run (aligned with the keyword arguments of upstream
/// `RichText.add()`).
///
/// `None` / `false` / empty string match upstream falsy semantics: the corresponding `w:rPr`
/// child element is not emitted. `size` keeps the upstream string form (filled into `w:val`
/// as-is; e.g. `"20"` means 10 points).
#[derive(Debug, Clone, Default)]
pub struct RichTextProps {
    /// Run style name → `<w:rStyle w:val="…"/>`.
    pub style: Option<String>,
    /// Foreground color, optionally with a `#` prefix (stripped on output) →
    /// `<w:color w:val="…"/>`.
    pub color: Option<String>,
    /// Highlight color, optionally with a `#` prefix → `<w:shd w:fill="…"/>`.
    pub highlight: Option<String>,
    /// Font size (a string of half-point values) → `<w:sz/>` + `<w:szCs/>`.
    pub size: Option<String>,
    /// Subscript → `<w:vertAlign w:val="subscript"/>`.
    pub subscript: bool,
    /// Superscript → `<w:vertAlign w:val="superscript"/>`.
    pub superscript: bool,
    /// Bold; with `rtl`, also emits `<w:bCs/>`.
    pub bold: bool,
    /// Italic; with `rtl`, also emits `<w:iCs/>`.
    pub italic: bool,
    /// Underline type; invalid values are normalized to `single`.
    pub underline: Option<String>,
    /// Strikethrough → `<w:strike/>`.
    pub strike: bool,
    /// Font name; supports the `"{region}:{font}"` regional syntax (e.g.
    /// `eastAsia:SimSun`).
    pub font: Option<String>,
    /// Hyperlink relationship ID; when non-empty the whole run is wrapped in
    /// `<w:hyperlink>`.
    pub url_id: Option<String>,
    /// Right-to-left → `<w:rtl w:val="true"/>`.
    pub rtl: bool,
    /// Language tag → `<w:lang w:val="…"/>`.
    pub lang: Option<String>,
}

impl RichTextProps {
    /// All properties defaulted (equivalent to upstream `add(text)` with no keyword
    /// arguments).
    pub fn new() -> Self {
        Self::default()
    }
}

/// Concatenates the `w:rPr` child elements in the fixed order of upstream `add()`; returns
/// an empty string when there are no properties.
fn build_prop(p: &RichTextProps) -> String {
    let mut prop = String::new();
    if let Some(style) = p.style.as_deref().filter(|s| !s.is_empty()) {
        prop.push_str(&format!("<w:rStyle w:val=\"{style}\"/>"));
    }
    if let Some(color) = p.color.as_deref().filter(|s| !s.is_empty()) {
        // The '#' prefix is optional: strip a single '#' on output
        let color = color.strip_prefix('#').unwrap_or(color);
        prop.push_str(&format!("<w:color w:val=\"{color}\"/>"));
    }
    if let Some(highlight) = p.highlight.as_deref().filter(|s| !s.is_empty()) {
        let highlight = highlight.strip_prefix('#').unwrap_or(highlight);
        prop.push_str(&format!("<w:shd w:fill=\"{highlight}\"/>"));
    }
    if let Some(size) = p.size.as_deref().filter(|s| !s.is_empty()) {
        prop.push_str(&format!(
            "<w:sz w:val=\"{size}\"/><w:szCs w:val=\"{size}\"/>"
        ));
    }
    if p.subscript {
        prop.push_str("<w:vertAlign w:val=\"subscript\"/>");
    }
    if p.superscript {
        prop.push_str("<w:vertAlign w:val=\"superscript\"/>");
    }
    if p.bold {
        prop.push_str("<w:b/>");
        if p.rtl {
            prop.push_str("<w:bCs/>");
        }
    }
    if p.italic {
        prop.push_str("<w:i/>");
        if p.rtl {
            prop.push_str("<w:iCs/>");
        }
    }
    if let Some(underline) = p.underline.as_deref().filter(|s| !s.is_empty()) {
        // Invalid values are normalized to single
        let val = if VALID_UNDERLINES.contains(&underline) {
            underline
        } else {
            "single"
        };
        prop.push_str(&format!("<w:u w:val=\"{val}\"/>"));
    }
    if p.strike {
        prop.push_str("<w:strike/>");
    }
    if let Some(font) = p.font.as_deref().filter(|s| !s.is_empty()) {
        // "region:Font" regional syntax: the regional font name also replaces the base
        // font, and the region name is appended as w:{region}="{font}" at the end of
        // w:rFonts
        let (font_name, regional) = match font.split_once(':') {
            Some((region, name)) => (name, format!(" w:{region}=\"{name}\"")),
            None => (font, String::new()),
        };
        prop.push_str(&format!(
            "<w:rFonts w:ascii=\"{font_name}\" w:hAnsi=\"{font_name}\" w:cs=\"{font_name}\"{regional}/>"
        ));
    }
    if p.rtl {
        prop.push_str("<w:rtl w:val=\"true\"/>");
    }
    if let Some(lang) = p.lang.as_deref().filter(|s| !s.is_empty()) {
        prop.push_str(&format!("<w:lang w:val=\"{lang}\"/>"));
    }
    prop
}

/// Generates the XML for a single run (including the hyperlink wrapper when `url_id` is
/// non-empty).
fn run_xml(text: &str, props: &RichTextProps) -> String {
    let escaped = escape_html(text);
    let prop = build_prop(props);
    let mut xml = String::from("<w:r>");
    // Upstream: emit w:rPr only when prop is non-empty
    if !prop.is_empty() {
        xml.push_str(&format!("<w:rPr>{prop}</w:rPr>"));
    }
    xml.push_str(&format!(
        "<w:t xml:space=\"preserve\">{escaped}</w:t></w:r>"
    ));
    // Upstream: wrap in a hyperlink only when url_id is non-empty (falsy check)
    if let Some(url_id) = props.url_id.as_deref().filter(|u| !u.is_empty()) {
        xml = format!("<w:hyperlink r:id=\"{url_id}\" w:tgtFrame=\"_blank\">{xml}</w:hyperlink>");
    }
    xml
}

/// Rich text: generates the `w:r` sequence injected into an existing paragraph (upstream
/// `RichText`).
///
/// # Examples
///
/// ```
/// let mut props = docxtpl_rich::RichTextProps::new();
/// props.bold = true;
/// let rt = docxtpl_rich::RichText::text_with("hi", &props);
/// assert_eq!(
///     rt.to_xml(),
///     "<w:r><w:rPr><w:b/></w:rPr><w:t xml:space=\"preserve\">hi</w:t></w:r>"
/// );
/// ```
#[derive(Debug, Clone, Default)]
pub struct RichText {
    /// Accumulated XML fragment.
    xml: String,
}

impl RichText {
    /// Empty construction (upstream `RichText()`): emits nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Upstream `RichText("text")`: a run without properties; empty strings are skipped as
    /// falsy.
    pub fn text(text: &str) -> Self {
        Self::text_with(text, &RichTextProps::new())
    }

    /// Upstream `RichText("text", bold=True, …)`: the first run already carries properties;
    /// empty strings are skipped.
    pub fn text_with(text: &str, props: &RichTextProps) -> Self {
        let mut rich = Self::new();
        if !text.is_empty() {
            rich.add_with(text, props);
        }
        rich
    }

    /// Upstream `rt.add("text")`: appends a run without properties.
    pub fn add(&mut self, text: &str) -> &mut Self {
        self.add_with(text, &RichTextProps::new())
    }

    /// Upstream `rt.add("text", bold=True, …)`: appends a run with properties.
    ///
    /// Note: as in upstream, `add` does **not** skip the empty string (it emits a run with an
    /// empty `<w:t>`); skipping happens only in the constructor's `if text:` branch.
    pub fn add_with(&mut self, text: &str, props: &RichTextProps) -> &mut Self {
        self.xml.push_str(&run_xml(text, props));
        self
    }

    /// Upstream `rt.add(other_rich_text)`: concatenates its XML directly (property arguments
    /// are ignored).
    pub fn add_rich(&mut self, other: &RichText) -> &mut Self {
        self.xml.push_str(&other.xml);
        self
    }

    /// The generated XML fragment.
    pub fn to_xml(&self) -> &str {
        &self.xml
    }
}

/// Rich-text paragraph: generates a standalone `w:p` (upstream `RichTextParagraph`, used
/// outside an existing paragraph).
#[derive(Debug, Clone, Default)]
pub struct RichTextParagraph {
    /// Accumulated XML fragment.
    xml: String,
}

impl RichTextParagraph {
    /// Empty construction (upstream `RichTextParagraph()`).
    pub fn new() -> Self {
        Self::default()
    }

    /// Upstream `RichTextParagraph("text", parastyle="…")`.
    pub fn with_text(text: &str, parastyle: &str) -> Self {
        let mut para = Self::new();
        // The upstream constructor uses `if text:`, so an empty string never calls add; this
        // differs from an explicit `add("")` after construction (which still yields an empty
        // paragraph).
        if !text.is_empty() {
            para.add(text, parastyle);
        }
        para
    }

    /// Constructs a paragraph from an existing [`RichText`].
    pub fn with_rich(rich: &RichText, parastyle: &str) -> Self {
        let mut para = Self::new();
        para.add_rich(rich, parastyle);
        para
    }

    /// Upstream `para.add("text", parastyle=…)`: plain text is first wrapped in a propertyless
    /// [`RichText`] (empty strings skipped), while the paragraph wrapper itself is always
    /// emitted.
    pub fn add(&mut self, text: &str, parastyle: &str) -> &mut Self {
        let rich = RichText::text(text);
        self.add_rich(&rich, parastyle)
    }

    /// Upstream `para.add(rich_text, parastyle=…)`.
    pub fn add_rich(&mut self, rich: &RichText, parastyle: &str) -> &mut Self {
        let mut xml = String::from("<w:p>");
        // Upstream: emit w:pPr only when parastyle is non-empty
        if !parastyle.is_empty() {
            xml.push_str(&format!("<w:pPr><w:pStyle w:val=\"{parastyle}\"/></w:pPr>"));
        }
        xml.push_str(rich.to_xml());
        xml.push_str("</w:p>");
        self.xml.push_str(&xml);
        self
    }

    /// The generated XML fragment.
    pub fn to_xml(&self) -> &str {
        &self.xml
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds props with a partial set of properties (test helper).
    fn props(build: impl FnOnce(&mut RichTextProps)) -> RichTextProps {
        let mut p = RichTextProps::new();
        build(&mut p);
        p
    }

    #[test]
    fn bold_color_size_attribute_order_matches_upstream() {
        let p = props(|p| {
            p.bold = true;
            p.color = Some("#FF0000".to_owned());
            p.size = Some("20".to_owned());
        });
        let rt = RichText::text_with("Chinese<b>bold", &p);
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:rPr><w:color w:val=\"FF0000\"/><w:sz w:val=\"20\"/>\
             <w:szCs w:val=\"20\"/><w:b/></w:rPr>\
             <w:t xml:space=\"preserve\">Chinese&lt;b&gt;bold</w:t></w:r>"
        );
    }

    #[test]
    fn empty_construction_and_empty_string_produce_nothing() {
        assert_eq!(RichText::new().to_xml(), "");
        assert_eq!(RichText::text("").to_xml(), "");
        assert_eq!(
            RichText::text_with("", &props(|p| p.bold = true)).to_xml(),
            ""
        );
    }

    #[test]
    fn add_empty_string_outputs_empty_run_matches_upstream() {
        // The empty-string check in upstream add() is commented out: an empty string still
        // emits an empty <w:t>
        let mut rt = RichText::new();
        rt.add("");
        assert_eq!(rt.to_xml(), "<w:r><w:t xml:space=\"preserve\"></w:t></w:r>");
    }

    #[test]
    fn multiple_runs_concat() {
        let mut rt = RichText::text("a");
        rt.add("b");
        rt.add("c");
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:t xml:space=\"preserve\">a</w:t></w:r>\
             <w:r><w:t xml:space=\"preserve\">b</w:t></w:r>\
             <w:r><w:t xml:space=\"preserve\">c</w:t></w:r>"
        );
    }

    #[test]
    fn url_id_wraps_hyperlink() {
        let p = props(|p| p.url_id = Some("rId9".to_owned()));
        let rt = RichText::text_with("link", &p);
        assert_eq!(
            rt.to_xml(),
            "<w:hyperlink r:id=\"rId9\" w:tgtFrame=\"_blank\">\
             <w:r><w:t xml:space=\"preserve\">link</w:t></w:r></w:hyperlink>"
        );
        // An empty url_id is as falsy as None: no wrapping
        let p = props(|p| p.url_id = Some(String::new()));
        let rt = RichText::text_with("link", &p);
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:t xml:space=\"preserve\">link</w:t></w:r>"
        );
    }

    #[test]
    fn regional_font_syntax() {
        let p = props(|p| p.font = Some("eastAsia:SimSun".to_owned()));
        let rt = RichText::text_with("regional font", &p);
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:rPr>\
             <w:rFonts w:ascii=\"SimSun\" w:hAnsi=\"SimSun\" w:cs=\"SimSun\" w:eastAsia=\"SimSun\"/>\
             </w:rPr><w:t xml:space=\"preserve\">regional font</w:t></w:r>"
        );
    }

    #[test]
    fn normal_font_syntax() {
        let p = props(|p| p.font = Some("Arial".to_owned()));
        let rt = RichText::text_with("text", &p);
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:rPr>\
             <w:rFonts w:ascii=\"Arial\" w:hAnsi=\"Arial\" w:cs=\"Arial\"/>\
             </w:rPr><w:t xml:space=\"preserve\">text</w:t></w:r>"
        );
    }

    #[test]
    fn underline_invalid_value_normalizes_to_single() {
        let p = props(|p| p.underline = Some("wavy2".to_owned()));
        let rt = RichText::text_with("underline", &p);
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:rPr><w:u w:val=\"single\"/></w:rPr>\
             <w:t xml:space=\"preserve\">underline</w:t></w:r>"
        );
    }

    #[test]
    fn underline_valid_value_passthrough() {
        let p = props(|p| p.underline = Some("double".to_owned()));
        let rt = RichText::text_with("underline", &p);
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:rPr><w:u w:val=\"double\"/></w:rPr>\
             <w:t xml:space=\"preserve\">underline</w:t></w:r>"
        );
    }

    // Test names keep the original OOXML element names (e.g. w:bCs, w:iCs, w:pPr), which are
    // not snake_case; the following allow can be removed if the project adopts English
    // snake_case test naming.
    #[allow(non_snake_case)]
    #[test]
    fn rtl_bold_outputs_b_and_b_cs() {
        let p = props(|p| {
            p.bold = true;
            p.rtl = true;
        });
        let rt = RichText::text_with("RTL bold", &p);
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:rPr><w:b/><w:bCs/><w:rtl w:val=\"true\"/></w:rPr>\
             <w:t xml:space=\"preserve\">RTL bold</w:t></w:r>"
        );
    }

    #[allow(non_snake_case)]
    #[test]
    fn rtl_italic_outputs_i_and_i_cs() {
        let p = props(|p| {
            p.italic = true;
            p.rtl = true;
        });
        let rt = RichText::text_with("RTL italic", &p);
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:rPr><w:i/><w:iCs/><w:rtl w:val=\"true\"/></w:rPr>\
             <w:t xml:space=\"preserve\">RTL italic</w:t></w:r>"
        );
    }

    #[test]
    fn superscript_with_language_and_subscript() {
        let p = props(|p| {
            p.superscript = true;
            p.lang = Some("zh-CN".to_owned());
        });
        let rt = RichText::text_with("superscript", &p);
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:rPr><w:vertAlign w:val=\"superscript\"/>\
             <w:lang w:val=\"zh-CN\"/></w:rPr>\
             <w:t xml:space=\"preserve\">superscript</w:t></w:r>"
        );

        let p = props(|p| p.subscript = true);
        let rt = RichText::text_with("subscript", &p);
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:rPr><w:vertAlign w:val=\"subscript\"/></w:rPr>\
             <w:t xml:space=\"preserve\">subscript</w:t></w:r>"
        );
    }

    #[test]
    fn highlight_hash_prefix_stripped() {
        let p = props(|p| p.highlight = Some("#FFFF00".to_owned()));
        let rt = RichText::text_with("highlight", &p);
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:rPr><w:shd w:fill=\"FFFF00\"/></w:rPr>\
             <w:t xml:space=\"preserve\">highlight</w:t></w:r>"
        );
    }

    #[test]
    fn style_strikethrough_and_italic() {
        let rt = RichText::text_with("style", &props(|p| p.style = Some("Emphasis".to_owned())));
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:rPr><w:rStyle w:val=\"Emphasis\"/></w:rPr>\
             <w:t xml:space=\"preserve\">style</w:t></w:r>"
        );

        let rt = RichText::text_with("strikethrough", &props(|p| p.strike = true));
        assert!(rt.to_xml().contains("<w:strike/>"));

        let rt = RichText::text_with("italic", &props(|p| p.italic = true));
        assert!(rt.to_xml().contains("<w:i/>"));
    }

    #[test]
    fn text_escaping_five_chars_full_coverage_matches_upstream() {
        // Upstream escape() is called without quote=False: single and double quotes are
        // escaped alike (verified by probe)
        let rt = RichText::text("a\"b'c&<>z");
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:t xml:space=\"preserve\">a&quot;b&#x27;c&amp;&lt;&gt;z</w:t></w:r>"
        );
    }

    #[test]
    fn add_rich_direct_concat() {
        let other = RichText::text_with("y", &props(|p| p.bold = true));
        let mut rt = RichText::text("x");
        rt.add_rich(&other);
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:t xml:space=\"preserve\">x</w:t></w:r>\
             <w:r><w:rPr><w:b/></w:rPr><w:t xml:space=\"preserve\">y</w:t></w:r>"
        );
    }

    #[test]
    fn all_properties_output_order_matches_upstream() {
        let p = props(|p| {
            p.style = Some("S".to_owned());
            p.color = Some("112233".to_owned());
            p.highlight = Some("FFFF00".to_owned());
            p.size = Some("30".to_owned());
            p.bold = true;
            p.italic = true;
            p.underline = Some("wave".to_owned());
            p.strike = true;
            p.font = Some("Arial".to_owned());
            p.rtl = true;
            p.lang = Some("fr-FR".to_owned());
        });
        let rt = RichText::text_with("full", &p);
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:rPr><w:rStyle w:val=\"S\"/><w:color w:val=\"112233\"/>\
             <w:shd w:fill=\"FFFF00\"/><w:sz w:val=\"30\"/><w:szCs w:val=\"30\"/>\
             <w:b/><w:bCs/><w:i/><w:iCs/><w:u w:val=\"wave\"/><w:strike/>\
             <w:rFonts w:ascii=\"Arial\" w:hAnsi=\"Arial\" w:cs=\"Arial\"/>\
             <w:rtl w:val=\"true\"/><w:lang w:val=\"fr-FR\"/></w:rPr>\
             <w:t xml:space=\"preserve\">full</w:t></w:r>"
        );
    }

    #[test]
    fn paragraph_style_wrap() {
        let para = RichTextParagraph::with_text("first paragraph", "ListBullet");
        assert_eq!(
            para.to_xml(),
            "<w:p><w:pPr><w:pStyle w:val=\"ListBullet\"/></w:pPr>\
             <w:r><w:t xml:space=\"preserve\">first paragraph</w:t></w:r></w:p>"
        );
    }

    #[allow(non_snake_case)]
    #[test]
    fn paragraph_no_style_emits_no_p_pr() {
        let para = RichTextParagraph::with_text("text", "");
        assert_eq!(
            para.to_xml(),
            "<w:p><w:r><w:t xml:space=\"preserve\">text</w:t></w:r></w:p>"
        );
    }

    #[test]
    fn paragraph_multiple_adds_form_separate_paragraphs() {
        let mut para = RichTextParagraph::with_text("a", "");
        para.add("b", "ListBullet");
        assert_eq!(
            para.to_xml(),
            "<w:p><w:r><w:t xml:space=\"preserve\">a</w:t></w:r></w:p>\
             <w:p><w:pPr><w:pStyle w:val=\"ListBullet\"/></w:pPr>\
             <w:r><w:t xml:space=\"preserve\">b</w:t></w:r></w:p>"
        );
    }

    #[test]
    fn paragraph_richtext_arg_and_add_rich() {
        let rich = RichText::text_with("rich", &props(|p| p.bold = true));
        let para = RichTextParagraph::with_rich(&rich, "");
        assert_eq!(
            para.to_xml(),
            "<w:p><w:r><w:rPr><w:b/></w:rPr>\
             <w:t xml:space=\"preserve\">rich</w:t></w:r></w:p>"
        );

        let mut para = RichTextParagraph::new();
        para.add_rich(&rich, "ListBullet");
        assert_eq!(
            para.to_xml(),
            "<w:p><w:pPr><w:pStyle w:val=\"ListBullet\"/></w:pPr>\
             <w:r><w:rPr><w:b/></w:rPr>\
             <w:t xml:space=\"preserve\">rich</w:t></w:r></w:p>"
        );
    }

    #[test]
    fn paragraph_empty_text_constructor_emits_nothing_explicit_add_still_outputs_empty_paragraph() {
        // docxtpl 0.20.2 oracle: RichTextParagraph("").xml == "", but
        // `para = RichTextParagraph(); para.add("")` outputs `<w:p></w:p>`.
        assert_eq!(RichTextParagraph::with_text("", "").to_xml(), "");
        assert_eq!(RichTextParagraph::with_text("", "ListBullet").to_xml(), "");
        assert_eq!(RichTextParagraph::new().to_xml(), "");

        let mut para = RichTextParagraph::new();
        para.add("", "");
        assert_eq!(para.to_xml(), "<w:p></w:p>");

        let mut styled = RichTextParagraph::new();
        styled.add("", "ListBullet");
        assert_eq!(
            styled.to_xml(),
            "<w:p><w:pPr><w:pStyle w:val=\"ListBullet\"/></w:pPr></w:p>"
        );
    }
}
