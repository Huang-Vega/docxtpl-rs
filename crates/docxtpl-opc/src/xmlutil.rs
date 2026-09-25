//! quick-xml 属性读取与手写 XML 序列化的小工具。

use quick_xml::events::BytesStart;

/// lxml `etree.tostring(element, encoding="UTF-8", standalone=True)` 的声明风格：
/// 单引号属性 + UTF-8 + standalone，末尾一个换行。
///
/// python-docx 保存包时由 lxml 重建 `[Content_Types].xml` 与 `.rels`，
/// 逐字节对齐 oracle 必须复用同一声明。
pub(crate) const LXML_XML_DECLARATION: &str =
    "<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n";

/// XML 属性值转义（与 lxml 默认序列化一致：`&`/`<`/`>`/`"` 四个实体）。
pub(crate) fn escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// 读取并解码一个可选 XML 属性。
///
/// 失败时返回错误说明字符串，由调用方包装成具体的 [`crate::OpcError`] 变体。
pub(crate) fn attr_value(element: &BytesStart<'_>, name: &str) -> Result<Option<String>, String> {
    match element.try_get_attribute(name) {
        Err(err) => Err(format!("读取属性 {name} 失败: {err}")),
        Ok(None) => Ok(None),
        Ok(Some(attribute)) => match attribute.unescape_value() {
            Ok(value) => Ok(Some(value.into_owned())),
            Err(err) => Err(format!("解码属性 {name} 失败: {err}")),
        },
    }
}

/// 读取并解码一个必需 XML 属性。
pub(crate) fn required_attr(element: &BytesStart<'_>, name: &str) -> Result<String, String> {
    attr_value(element, name)?.ok_or_else(|| format!("缺少必需属性 {name}"))
}
