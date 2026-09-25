//! Listing：转义文本值（对齐 docxtpl 0.20.2 `listing.py`）。

use crate::richtext::escape_html;

/// 转义后的纯文本值（上游 `Listing`）。
///
/// 只做与上游一致的 `html.escape` 转义；换行等控制符的 OOXML 展开
/// （`<w:br/>` 等）由渲染管线的 resolve_listing 阶段完成（见
/// docxtpl-template / docxtpl-compat）。
///
/// # 示例
///
/// ```
/// let l = docxtpl_rich::Listing::new("a&b<c>");
/// assert_eq!(l.to_xml(), "a&amp;b&lt;c&gt;");
/// ```
#[derive(Debug, Clone, Default)]
pub struct Listing {
    /// 转义后的 XML 文本。
    xml: String,
}

impl Listing {
    /// 构造：对文本做与上游 `escape(text)` 一致的转义。
    ///
    /// 上游对非字符串输入会先 `str()`；Rust 侧由调用方自行格式化为字符串。
    pub fn new(text: &str) -> Self {
        Self {
            xml: escape_html(text),
        }
    }

    /// 转义后的 XML 文本。
    pub fn to_xml(&self) -> &str {
        &self.xml
    }
}

#[cfg(test)]
mod tests {
    use super::Listing;

    #[test]
    fn 换行原样保留() {
        // 上游 Listing 不处理 \n（由 resolve_listing 展开），原样保留
        assert_eq!(Listing::new("a\nb").to_xml(), "a\nb");
    }

    #[test]
    fn 特殊字符转义() {
        assert_eq!(Listing::new("a&b<c>").to_xml(), "a&amp;b&lt;c&gt;");
    }

    #[test]
    fn 引号同样转义_对齐上游() {
        // 上游 escape() 未传 quote=False（探针实证）
        assert_eq!(Listing::new("引\"号'").to_xml(), "引&quot;号&#x27;");
    }

    #[test]
    fn 空文本() {
        assert_eq!(Listing::new("").to_xml(), "");
    }
}
