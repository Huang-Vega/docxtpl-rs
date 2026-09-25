//! RichText / RichTextParagraph：字节级对齐 docxtpl 0.20.2 `richtext.py`。
//!
//! - run 模板：`<w:r>` +（属性非空时 `<w:rPr>…</w:rPr>`）+
//!   `<w:t xml:space="preserve">…</w:t></w:r>`；
//! - 属性按上游 `add()` 的固定顺序拼接（rStyle → color → shd → sz/szCs →
//!   vertAlign → b/bCs → i/iCs → u → strike → rFonts → rtl → lang）；
//! - 空串语义（探针实证）：构造函数 `if text:` falsy 跳过；`add()` 对空串
//!   **不**跳过（输出空 `<w:t>`，上游注释掉了该检查）。

/// 与 Python `html.escape(text)`（quote 缺省为 True）逐字节一致的转义。
///
/// 注意：上游 `richtext.py` / `listing.py` 调用 `escape(text)` 时未传
/// `quote=False`，因此五个字符都转义。探针实证：
/// `RichText('引"号\'与&<>尾').xml` 输出 `引&quot;号&#x27;与&amp;&lt;&gt;尾`。
pub(crate) fn escape_html(text: &str) -> String {
    // 顺序与 CPython html.escape 一致：& 最先替换，避免替换产物被二次转义
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// `w:u w:val` 的合法取值集合（上游 add() 内的硬编码列表）。
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

/// 单个 run 的富文本属性（对齐上游 `RichText.add()` 的关键字参数）。
///
/// `None` / `false` / 空串与上游 falsy 语义一致：不输出对应 `w:rPr` 子元素。
/// `size` 沿用上游的字符串形态（原样填入 `w:val`，如 `"20"` 表示 10 磅）。
#[derive(Debug, Clone, Default)]
pub struct RichTextProps {
    /// run 样式名 → `<w:rStyle w:val="…"/>`。
    pub style: Option<String>,
    /// 前景色，可带 `#` 前缀（输出时剥离）→ `<w:color w:val="…"/>`。
    pub color: Option<String>,
    /// 高亮色，可带 `#` 前缀 → `<w:shd w:fill="…"/>`。
    pub highlight: Option<String>,
    /// 字号（半磅值的字符串）→ `<w:sz/>` + `<w:szCs/>`。
    pub size: Option<String>,
    /// 下标 → `<w:vertAlign w:val="subscript"/>`。
    pub subscript: bool,
    /// 上标 → `<w:vertAlign w:val="superscript"/>`。
    pub superscript: bool,
    /// 粗体；`rtl` 时同时输出 `<w:bCs/>`。
    pub bold: bool,
    /// 斜体；`rtl` 时同时输出 `<w:iCs/>`。
    pub italic: bool,
    /// 下划线类型；非法值归一为 `single`。
    pub underline: Option<String>,
    /// 删除线 → `<w:strike/>`。
    pub strike: bool,
    /// 字体名，支持 `"{region}:{font}"` 区域语法（如 `eastAsia:SimSun`）。
    pub font: Option<String>,
    /// 超链接关系 ID，非空时整个 run 包裹 `<w:hyperlink>`。
    pub url_id: Option<String>,
    /// 从右到左 → `<w:rtl w:val="true"/>`。
    pub rtl: bool,
    /// 语言标记 → `<w:lang w:val="…"/>`。
    pub lang: Option<String>,
}

impl RichTextProps {
    /// 全部属性缺省（等价上游 `add(text)` 不带任何关键字参数）。
    pub fn new() -> Self {
        Self::default()
    }
}

/// 按上游 `add()` 的固定顺序拼接 `w:rPr` 子元素；无属性时返回空串。
fn build_prop(p: &RichTextProps) -> String {
    let mut prop = String::new();
    if let Some(style) = p.style.as_deref().filter(|s| !s.is_empty()) {
        prop.push_str(&format!("<w:rStyle w:val=\"{style}\"/>"));
    }
    if let Some(color) = p.color.as_deref().filter(|s| !s.is_empty()) {
        // '#' 前缀可省：输出时剥离一个 '#'
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
        // 非法值归一为 single
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
        // "region:Font" 区域语法：区域字体名同时替换基础字体，
        // 区域名以 w:{region}="{font}" 追加在 w:rFonts 尾部
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

/// 生成单个 run 的 XML（含 `url_id` 非空时的超链接包裹）。
fn run_xml(text: &str, props: &RichTextProps) -> String {
    let escaped = escape_html(text);
    let prop = build_prop(props);
    let mut xml = String::from("<w:r>");
    // 上游：prop 非空才输出 w:rPr
    if !prop.is_empty() {
        xml.push_str(&format!("<w:rPr>{prop}</w:rPr>"));
    }
    xml.push_str(&format!(
        "<w:t xml:space=\"preserve\">{escaped}</w:t></w:r>"
    ));
    // 上游：url_id 非空（falsy 检查）才包裹超链接
    if let Some(url_id) = props.url_id.as_deref().filter(|u| !u.is_empty()) {
        xml = format!("<w:hyperlink r:id=\"{url_id}\" w:tgtFrame=\"_blank\">{xml}</w:hyperlink>");
    }
    xml
}

/// 富文本：生成注入既有段落的 `w:r` 序列（上游 `RichText`）。
///
/// # 示例
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
    /// 累积的 XML 片段。
    xml: String,
}

impl RichText {
    /// 空构造（上游 `RichText()`）：不输出任何内容。
    pub fn new() -> Self {
        Self::default()
    }

    /// 上游 `RichText("文本")`：无属性 run；空串按 falsy 跳过。
    pub fn text(text: &str) -> Self {
        Self::text_with(text, &RichTextProps::new())
    }

    /// 上游 `RichText("文本", bold=True, …)`：首个 run 即带属性；空串跳过。
    pub fn text_with(text: &str, props: &RichTextProps) -> Self {
        let mut rich = Self::new();
        if !text.is_empty() {
            rich.add_with(text, props);
        }
        rich
    }

    /// 上游 `rt.add("文本")`：追加无属性 run。
    pub fn add(&mut self, text: &str) -> &mut Self {
        self.add_with(text, &RichTextProps::new())
    }

    /// 上游 `rt.add("文本", bold=True, …)`：追加带属性 run。
    ///
    /// 注意：与上游一致，`add` 对空串**不**跳过（输出空 `<w:t>` 的 run）；
    /// 跳过只发生在构造函数的 `if text:` 分支。
    pub fn add_with(&mut self, text: &str, props: &RichTextProps) -> &mut Self {
        self.xml.push_str(&run_xml(text, props));
        self
    }

    /// 上游 `rt.add(其他RichText)`：直接拼接其 XML（忽略属性参数）。
    pub fn add_rich(&mut self, other: &RichText) -> &mut Self {
        self.xml.push_str(&other.xml);
        self
    }

    /// 生成的 XML 片段。
    pub fn to_xml(&self) -> &str {
        &self.xml
    }
}

/// 富文本段落：生成独立 `w:p`（上游 `RichTextParagraph`，用于既有段落之外）。
#[derive(Debug, Clone, Default)]
pub struct RichTextParagraph {
    /// 累积的 XML 片段。
    xml: String,
}

impl RichTextParagraph {
    /// 空构造（上游 `RichTextParagraph()`）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 上游 `RichTextParagraph("文本", parastyle="…")`。
    pub fn with_text(text: &str, parastyle: &str) -> Self {
        let mut para = Self::new();
        para.add(text, parastyle);
        para
    }

    /// 以现成 [`RichText`] 构造段落。
    pub fn with_rich(rich: &RichText, parastyle: &str) -> Self {
        let mut para = Self::new();
        para.add_rich(rich, parastyle);
        para
    }

    /// 上游 `para.add("文本", parastyle=…)`：普通文本先经无属性
    /// [`RichText`] 构造（空串跳过），段落包装本身总是输出。
    pub fn add(&mut self, text: &str, parastyle: &str) -> &mut Self {
        let rich = RichText::text(text);
        self.add_rich(&rich, parastyle)
    }

    /// 上游 `para.add(富文本, parastyle=…)`。
    pub fn add_rich(&mut self, rich: &RichText, parastyle: &str) -> &mut Self {
        let mut xml = String::from("<w:p>");
        // 上游：parastyle 非空才输出 w:pPr
        if !parastyle.is_empty() {
            xml.push_str(&format!("<w:pPr><w:pStyle w:val=\"{parastyle}\"/></w:pPr>"));
        }
        xml.push_str(rich.to_xml());
        xml.push_str("</w:p>");
        self.xml.push_str(&xml);
        self
    }

    /// 生成的 XML 片段。
    pub fn to_xml(&self) -> &str {
        &self.xml
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造带部分属性的 props（测试辅助）。
    fn props(build: impl FnOnce(&mut RichTextProps)) -> RichTextProps {
        let mut p = RichTextProps::new();
        build(&mut p);
        p
    }

    #[test]
    fn 加粗颜色字号_属性顺序对齐上游() {
        let p = props(|p| {
            p.bold = true;
            p.color = Some("#FF0000".to_owned());
            p.size = Some("20".to_owned());
        });
        let rt = RichText::text_with("中文<b>粗", &p);
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:rPr><w:color w:val=\"FF0000\"/><w:sz w:val=\"20\"/>\
             <w:szCs w:val=\"20\"/><w:b/></w:rPr>\
             <w:t xml:space=\"preserve\">中文&lt;b&gt;粗</w:t></w:r>"
        );
    }

    #[test]
    fn 空构造与空串构造_不输出() {
        assert_eq!(RichText::new().to_xml(), "");
        assert_eq!(RichText::text("").to_xml(), "");
        assert_eq!(
            RichText::text_with("", &props(|p| p.bold = true)).to_xml(),
            ""
        );
    }

    #[test]
    fn add空串_输出空run_对齐上游() {
        // 上游 add() 的空串检查被注释掉：空串照样输出空 <w:t>
        let mut rt = RichText::new();
        rt.add("");
        assert_eq!(rt.to_xml(), "<w:r><w:t xml:space=\"preserve\"></w:t></w:r>");
    }

    #[test]
    fn 多run拼接() {
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
    fn url_id_包裹超链接() {
        let p = props(|p| p.url_id = Some("rId9".to_owned()));
        let rt = RichText::text_with("链接", &p);
        assert_eq!(
            rt.to_xml(),
            "<w:hyperlink r:id=\"rId9\" w:tgtFrame=\"_blank\">\
             <w:r><w:t xml:space=\"preserve\">链接</w:t></w:r></w:hyperlink>"
        );
        // 空 url_id 与 None 同为 falsy：不包裹
        let p = props(|p| p.url_id = Some(String::new()));
        let rt = RichText::text_with("链接", &p);
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:t xml:space=\"preserve\">链接</w:t></w:r>"
        );
    }

    #[test]
    fn 区域字体语法() {
        let p = props(|p| p.font = Some("eastAsia:SimSun".to_owned()));
        let rt = RichText::text_with("区域字体", &p);
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:rPr>\
             <w:rFonts w:ascii=\"SimSun\" w:hAnsi=\"SimSun\" w:cs=\"SimSun\" w:eastAsia=\"SimSun\"/>\
             </w:rPr><w:t xml:space=\"preserve\">区域字体</w:t></w:r>"
        );
    }

    #[test]
    fn 普通字体语法() {
        let p = props(|p| p.font = Some("Arial".to_owned()));
        let rt = RichText::text_with("文本", &p);
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:rPr>\
             <w:rFonts w:ascii=\"Arial\" w:hAnsi=\"Arial\" w:cs=\"Arial\"/>\
             </w:rPr><w:t xml:space=\"preserve\">文本</w:t></w:r>"
        );
    }

    #[test]
    fn 下划线_非法值归一为single() {
        let p = props(|p| p.underline = Some("wavy2".to_owned()));
        let rt = RichText::text_with("下划线", &p);
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:rPr><w:u w:val=\"single\"/></w:rPr>\
             <w:t xml:space=\"preserve\">下划线</w:t></w:r>"
        );
    }

    #[test]
    fn 下划线_合法值透传() {
        let p = props(|p| p.underline = Some("double".to_owned()));
        let rt = RichText::text_with("下划线", &p);
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:rPr><w:u w:val=\"double\"/></w:rPr>\
             <w:t xml:space=\"preserve\">下划线</w:t></w:r>"
        );
    }

    // 测试名保留 OOXML 元素原名（如 w:bCs、w:iCs、w:pPr），不属于 snake_case；
    // 若项目改用英文 snake_case 测试命名规范，可移除以下 allow。
    #[allow(non_snake_case)]
    #[test]
    fn rtl加粗_输出b与bCs() {
        let p = props(|p| {
            p.bold = true;
            p.rtl = true;
        });
        let rt = RichText::text_with("RTL粗", &p);
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:rPr><w:b/><w:bCs/><w:rtl w:val=\"true\"/></w:rPr>\
             <w:t xml:space=\"preserve\">RTL粗</w:t></w:r>"
        );
    }

    #[allow(non_snake_case)]
    #[test]
    fn rtl斜体_输出i与iCs() {
        let p = props(|p| {
            p.italic = true;
            p.rtl = true;
        });
        let rt = RichText::text_with("RTL斜", &p);
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:rPr><w:i/><w:iCs/><w:rtl w:val=\"true\"/></w:rPr>\
             <w:t xml:space=\"preserve\">RTL斜</w:t></w:r>"
        );
    }

    #[test]
    fn 上标带语言与下标() {
        let p = props(|p| {
            p.superscript = true;
            p.lang = Some("zh-CN".to_owned());
        });
        let rt = RichText::text_with("上标", &p);
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:rPr><w:vertAlign w:val=\"superscript\"/>\
             <w:lang w:val=\"zh-CN\"/></w:rPr>\
             <w:t xml:space=\"preserve\">上标</w:t></w:r>"
        );

        let p = props(|p| p.subscript = true);
        let rt = RichText::text_with("下标", &p);
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:rPr><w:vertAlign w:val=\"subscript\"/></w:rPr>\
             <w:t xml:space=\"preserve\">下标</w:t></w:r>"
        );
    }

    #[test]
    fn 高亮_井号前缀剥离() {
        let p = props(|p| p.highlight = Some("#FFFF00".to_owned()));
        let rt = RichText::text_with("高亮", &p);
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:rPr><w:shd w:fill=\"FFFF00\"/></w:rPr>\
             <w:t xml:space=\"preserve\">高亮</w:t></w:r>"
        );
    }

    #[test]
    fn 样式与删除线与斜体() {
        let rt = RichText::text_with("样式", &props(|p| p.style = Some("Emphasis".to_owned())));
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:rPr><w:rStyle w:val=\"Emphasis\"/></w:rPr>\
             <w:t xml:space=\"preserve\">样式</w:t></w:r>"
        );

        let rt = RichText::text_with("删除线", &props(|p| p.strike = true));
        assert!(rt.to_xml().contains("<w:strike/>"));

        let rt = RichText::text_with("斜体", &props(|p| p.italic = true));
        assert!(rt.to_xml().contains("<w:i/>"));
    }

    #[test]
    fn 文本转义_五字符全覆盖_对齐上游() {
        // 上游 escape() 未传 quote=False：单双引号同样转义（探针实证）
        let rt = RichText::text("引\"号'与&<>尾");
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:t xml:space=\"preserve\">引&quot;号&#x27;与&amp;&lt;&gt;尾</w:t></w:r>"
        );
    }

    #[test]
    fn add_rich_直接拼接() {
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
    fn 全属性_输出顺序对齐上游() {
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
        let rt = RichText::text_with("全", &p);
        assert_eq!(
            rt.to_xml(),
            "<w:r><w:rPr><w:rStyle w:val=\"S\"/><w:color w:val=\"112233\"/>\
             <w:shd w:fill=\"FFFF00\"/><w:sz w:val=\"30\"/><w:szCs w:val=\"30\"/>\
             <w:b/><w:bCs/><w:i/><w:iCs/><w:u w:val=\"wave\"/><w:strike/>\
             <w:rFonts w:ascii=\"Arial\" w:hAnsi=\"Arial\" w:cs=\"Arial\"/>\
             <w:rtl w:val=\"true\"/><w:lang w:val=\"fr-FR\"/></w:rPr>\
             <w:t xml:space=\"preserve\">全</w:t></w:r>"
        );
    }

    #[test]
    fn 段落_样式包裹() {
        let para = RichTextParagraph::with_text("首段", "ListBullet");
        assert_eq!(
            para.to_xml(),
            "<w:p><w:pPr><w:pStyle w:val=\"ListBullet\"/></w:pPr>\
             <w:r><w:t xml:space=\"preserve\">首段</w:t></w:r></w:p>"
        );
    }

    #[allow(non_snake_case)]
    #[test]
    fn 段落_无样式不输出pPr() {
        let para = RichTextParagraph::with_text("文本", "");
        assert_eq!(
            para.to_xml(),
            "<w:p><w:r><w:t xml:space=\"preserve\">文本</w:t></w:r></w:p>"
        );
    }

    #[test]
    fn 段落_多次add各成一段() {
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
    fn 段落_富文本入参与add_rich() {
        let rich = RichText::text_with("富", &props(|p| p.bold = true));
        let para = RichTextParagraph::with_rich(&rich, "");
        assert_eq!(
            para.to_xml(),
            "<w:p><w:r><w:rPr><w:b/></w:rPr>\
             <w:t xml:space=\"preserve\">富</w:t></w:r></w:p>"
        );

        let mut para = RichTextParagraph::new();
        para.add_rich(&rich, "ListBullet");
        assert_eq!(
            para.to_xml(),
            "<w:p><w:pPr><w:pStyle w:val=\"ListBullet\"/></w:pPr>\
             <w:r><w:rPr><w:b/></w:rPr>\
             <w:t xml:space=\"preserve\">富</w:t></w:r></w:p>"
        );
    }

    #[test]
    fn 段落_空文本输出空段落() {
        // 空文本经 RichText 构造后为空，段落包装仍输出（上游语义）
        assert_eq!(RichTextParagraph::with_text("", "").to_xml(), "<w:p></w:p>");
        assert_eq!(RichTextParagraph::new().to_xml(), "");

        let mut para = RichTextParagraph::new();
        para.add("", "ListBullet");
        assert_eq!(
            para.to_xml(),
            "<w:p><w:pPr><w:pStyle w:val=\"ListBullet\"/></w:pPr></w:p>"
        );
    }
}
