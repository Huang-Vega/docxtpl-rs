//! quick-xml 属性读取的小工具。

use quick_xml::events::BytesStart;

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
