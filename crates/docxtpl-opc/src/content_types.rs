//! [Content_Types].xml 的解析与查询。

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::error::OpcError;
use crate::uri::PartUri;
use crate::xmlutil::required_attr;

/// Content Types：`Default`（按扩展名）+ `Override`（按 part 名）。
///
/// 查询规则：先查 `Override`（part 名精确匹配，大小写敏感），
/// 未命中再按文件扩展名查 `Default`（扩展名大小写不敏感）。
///
/// # 失败情况
///
/// [`ContentTypes::parse`] 在 XML 非法或缺少必需属性时返回
/// [`OpcError::InvalidContentTypes`]；`content_type_of` 无匹配时返回 `None`。
///
/// # 示例
///
/// ```
/// use docxtpl_opc::{ContentTypes, PartUri};
///
/// let xml = r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
///   <Default Extension="xml" ContentType="application/xml"/>
///   <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
/// </Types>"#;
/// let ct = ContentTypes::parse(xml)?;
/// let doc = PartUri::new("word/document.xml")?;
/// assert_eq!(
///     ct.content_type_of(&doc),
///     Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml")
/// );
/// assert_eq!(ct.content_type_of(&PartUri::new("word/styles.XML")?), Some("application/xml"));
/// assert_eq!(ct.content_type_of(&PartUri::new("media/image.png")?), None);
/// # Ok::<(), docxtpl_opc::OpcError>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContentTypes {
    /// (扩展名小写, content type)，保持文件顺序。
    defaults: Vec<(String, String)>,
    /// (part 名，无前导 /, content type)，保持文件顺序。
    overrides: Vec<(String, String)>,
}

impl ContentTypes {
    /// 解析 [Content_Types].xml 文本。
    ///
    /// # 失败情况
    ///
    /// [`OpcError::InvalidContentTypes`]：XML 非法、`Default`/`Override` 缺少
    /// 必需属性（Extension/ContentType/PartName）或属性值为空。
    pub fn parse(xml: &str) -> Result<Self, OpcError> {
        fn invalid(reason: String) -> OpcError {
            OpcError::InvalidContentTypes { reason }
        }

        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut types = ContentTypes::default();
        loop {
            let event = reader
                .read_event()
                .map_err(|err| invalid(format!("XML 解析失败: {err}")))?;
            let element = match event {
                Event::Start(element) | Event::Empty(element) => element,
                Event::Eof => break,
                _ => continue,
            };
            match element.local_name().as_ref() {
                b"Default" => {
                    let extension = required_attr(&element, "Extension").map_err(invalid)?;
                    let content_type = required_attr(&element, "ContentType").map_err(invalid)?;
                    if extension.is_empty() || content_type.is_empty() {
                        return Err(invalid(
                            "Default 的 Extension/ContentType 不能为空".to_string(),
                        ));
                    }
                    // 扩展名匹配大小写不敏感（OPC 规则），统一小写存储。
                    types
                        .defaults
                        .push((extension.to_lowercase(), content_type));
                }
                // OPC PartName 常带前导 /，part 名本身无前导 /。
                b"Override" => {
                    let part_name = required_attr(&element, "PartName").map_err(invalid)?;
                    let content_type = required_attr(&element, "ContentType").map_err(invalid)?;
                    if part_name.is_empty() || content_type.is_empty() {
                        return Err(invalid(
                            "Override 的 PartName/ContentType 不能为空".to_string(),
                        ));
                    }
                    let name = part_name.strip_prefix('/').unwrap_or(part_name.as_str());
                    types.overrides.push((name.to_string(), content_type));
                }
                _ => {}
            }
        }
        Ok(types)
    }

    /// 查询 part 的 content type：`Override` 优先，其次按扩展名查 `Default`。
    ///
    /// 无匹配返回 `None`。
    pub fn content_type_of(&self, uri: &PartUri) -> Option<&str> {
        let name = uri.as_str();
        for (part, content_type) in &self.overrides {
            if part == name {
                return Some(content_type);
            }
        }
        let file_name = uri.file_name();
        let extension = file_name.rfind('.').map(|dot| &file_name[dot + 1..])?;
        let extension = extension.to_lowercase();
        for (known, content_type) in &self.defaults {
            if *known == extension {
                return Some(content_type);
            }
        }
        None
    }

    /// 是否已登记某扩展名的 `Default`（大小写不敏感）。
    pub fn has_default(&self, extension: &str) -> bool {
        let extension = extension.to_lowercase();
        self.defaults.iter().any(|(known, _)| *known == extension)
    }

    /// 已登记的全部 `Default`（扩展名小写，保持文件序）。
    pub fn defaults(&self) -> impl Iterator<Item = (&str, &str)> {
        self.defaults
            .iter()
            .map(|(ext, ct)| (ext.as_str(), ct.as_str()))
    }

    /// 追加一个 `Default`（扩展名统一小写）；同名扩展名已存在时忽略，
    /// 对齐上游 `CaseInsensitiveDict.__setitem__` 的覆盖去重行为
    /// （模板内 content type 固定，不会出现同扩展名异 content type）。
    pub fn add_default(&mut self, extension: &str, content_type: &str) {
        let extension = extension.to_lowercase();
        if self.has_default(&extension) {
            return;
        }
        self.defaults.push((extension, content_type.to_string()));
    }

    /// 新增或替换一条 Override 声明（`part_name` 允许带或不带前导 `/`）。
    ///
    /// 供 Subdoc 部件合并使用：搬入的外部 docx 部件（styles/numbering/
    /// header/footer 等）按部件名声明内容类型。
    pub fn add_override(&mut self, part_name: &str, content_type: &str) {
        let name = part_name.strip_prefix('/').unwrap_or(part_name);
        if let Some(slot) = self.overrides.iter_mut().find(|(p, _)| p == name) {
            slot.1 = content_type.to_string();
            return;
        }
        self.overrides
            .push((name.to_string(), content_type.to_string()));
    }

    /// 序列化为 python-docx 保存时的 `[Content_Types].xml` 字节。
    ///
    /// 格式钉死 lxml 输出：单引号 XML 声明 + `\n`；根元素 `Types`（包内容
    /// 类型命名空间）；`Default` 按 Extension、`Override` 按 PartName 分别
    /// ASCII 排序（上游 `sorted(self._defaults)`/`sorted(self._overrides)`），
    /// 子元素无空白；`Override` 的 PartName 带前导 `/`。
    ///
    /// # 示例
    ///
    /// ```
    /// # use docxtpl_opc::ContentTypes;
    /// let mut ct = ContentTypes::default();
    /// ct.add_default("xml", "application/xml");
    /// ct.add_default("rels", "application/vnd.openxmlformats-package.relationships+xml");
    /// assert_eq!(
    ///     ct.to_xml(),
    ///     "<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n\
    ///      <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
    ///      <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
    ///      <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
    ///      </Types>"
    /// );
    /// ```
    pub fn to_xml(&self) -> String {
        use crate::xmlutil::{escape_attribute, LXML_XML_DECLARATION};

        const CONTENT_TYPES_NS: &str =
            "http://schemas.openxmlformats.org/package/2006/content-types";

        let mut defaults = self.defaults.iter().collect::<Vec<_>>();
        defaults.sort_by(|a, b| a.0.cmp(&b.0));
        let mut overrides = self.overrides.iter().collect::<Vec<_>>();
        overrides.sort_by(|a, b| a.0.cmp(&b.0));

        let empty = defaults.is_empty() && overrides.is_empty();
        let mut out = String::from(LXML_XML_DECLARATION);
        if empty {
            out.push_str(&format!(r#"<Types xmlns="{CONTENT_TYPES_NS}"/>"#));
            return out;
        }
        out.push_str(&format!(r#"<Types xmlns="{CONTENT_TYPES_NS}">"#));
        for (extension, content_type) in defaults {
            out.push_str(&format!(
                r#"<Default Extension="{}" ContentType="{}"/>"#,
                escape_attribute(extension),
                escape_attribute(content_type),
            ));
        }
        for (part_name, content_type) in overrides {
            out.push_str(&format!(
                r#"<Override PartName="/{}" ContentType="{}"/>"#,
                escape_attribute(part_name),
                escape_attribute(content_type),
            ));
        }
        out.push_str("</Types>");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="XML" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#;

    #[test]
    fn parses_and_looks_up() {
        let ct = ContentTypes::parse(SAMPLE).unwrap();
        let doc = PartUri::new("word/document.xml").unwrap();
        assert_eq!(
            ct.content_type_of(&doc),
            Some(
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"
            )
        );
        // Override 未命中时按扩展名查 Default（大小写不敏感）
        let styles = PartUri::new("word/styles.xml").unwrap();
        assert_eq!(ct.content_type_of(&styles), Some("application/xml"));
        let rels = PartUri::new("_rels/.rels").unwrap();
        assert_eq!(
            ct.content_type_of(&rels),
            Some("application/vnd.openxmlformats-package.relationships+xml")
        );
        assert_eq!(
            ct.content_type_of(&PartUri::new("media/image.png").unwrap()),
            None
        );
        // 无扩展名
        assert_eq!(ct.content_type_of(&PartUri::new("README").unwrap()), None);
    }

    #[test]
    fn rejects_missing_or_empty_attributes() {
        for bad in [
            "<Types><Default ContentType=\"application/xml\"/></Types>",
            "<Types><Default Extension=\"xml\"/></Types>",
            "<Types><Default Extension=\"\" ContentType=\"application/xml\"/></Types>",
            "<Types><Override PartName=\"/a.xml\"/></Types>",
            "<Types><Override ContentType=\"application/xml\"/></Types>",
            "<Types><Override PartName=\"\" ContentType=\"application/xml\"/></Types>",
        ] {
            let err = ContentTypes::parse(bad).unwrap_err();
            assert!(
                matches!(err, OpcError::InvalidContentTypes { .. }),
                "{bad}: {err:?}"
            );
        }
    }

    #[test]
    fn rejects_broken_xml() {
        let err =
            ContentTypes::parse("<Types><Default Extension=\"xml\" ContentType=\"a\"></Wrong>")
                .unwrap_err();
        assert!(matches!(err, OpcError::InvalidContentTypes { .. }));
    }

    #[test]
    fn add_default_dedupes_case_insensitively() {
        let mut ct = ContentTypes::default();
        ct.add_default("PNG", "image/png");
        ct.add_default("png", "image/png-other");
        // 第二个 add 被忽略：只有一个 Default，content type 保持首个值。
        assert_eq!(ct.to_xml().matches("<Default ").count(), 1);
        assert!(ct.has_default("pNg"));
        assert_eq!(
            ct.content_type_of(&PartUri::new("a.png").unwrap()),
            Some("image/png")
        );
    }

    #[test]
    fn to_xml_sorts_defaults_and_overrides() {
        // 对齐 p4_img_wh 的 CT：jpeg 必须排在 jpg 前（ASCII 'e' < 'g'），
        // png 之后；Override 按 PartName 排序、PartName 带前导 /。
        let xml = r#"<?xml version='1.0' encoding='UTF-8' standalone='yes'?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="jpg" ContentType="image/jpeg"/>
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="jpeg" ContentType="image/jpeg"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
<Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/>
</Types>"#;
        let mut ct = ContentTypes::parse(xml).unwrap();
        ct.add_default("png", "image/png");
        assert_eq!(
            ct.to_xml(),
            "<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"jpeg\" ContentType=\"image/jpeg\"/>\
<Default Extension=\"jpg\" ContentType=\"image/jpeg\"/>\
<Default Extension=\"png\" ContentType=\"image/png\"/>\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/>\
<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
</Types>"
        );

        // 再解析回来：逻辑内容一致（解析保持文件序，排序只发生在 to_xml）。
        let reparsed = ContentTypes::parse(&ct.to_xml()).unwrap();
        for ext in ["jpeg", "jpg", "png", "rels", "xml"] {
            assert!(reparsed.has_default(ext), "{ext}");
        }
        assert_eq!(
            reparsed.content_type_of(&PartUri::new("word/document.xml").unwrap()),
            Some(
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"
            )
        );
    }

    #[test]
    fn empty_types_serialize_self_closing() {
        assert_eq!(
            ContentTypes::default().to_xml(),
            "<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"/>"
        );
    }
}
