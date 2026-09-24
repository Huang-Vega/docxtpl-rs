//! OPC 关系（.rels 文件）。

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::error::OpcError;
use crate::uri::{resolve_relative_to, PartUri};
use crate::xmlutil::{attr_value, required_attr};

/// 关系目标模式。
///
/// # 示例
///
/// ```
/// use docxtpl_opc::TargetMode;
///
/// assert_ne!(TargetMode::Internal, TargetMode::External);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetMode {
    /// 包内目标（`TargetMode` 缺省值）。
    Internal,
    /// 包外目标（`TargetMode="External"`），不参与包内解析与存在性校验。
    External,
}

/// 单条关系（字段公开，可直接读取）。
///
/// # 示例
///
/// ```
/// use docxtpl_opc::{Relationship, TargetMode};
///
/// let rel = Relationship {
///     id: "rId1".to_string(),
///     rel_type: "http://example.com/rel".to_string(),
///     target: "styles.xml".to_string(),
///     target_mode: TargetMode::Internal,
/// };
/// assert_eq!(rel.id, "rId1");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relationship {
    /// 关系 Id；在同一个 .rels 文件内应当唯一（[`crate::Package::validate`] 检查）。
    pub id: String,
    /// 关系类型 URI。
    pub rel_type: String,
    /// 目标：内部关系为相对 owner 所在目录的路径（或以 `/` 开头的包内绝对路径）；
    /// 外部关系为任意 URI。
    pub target: String,
    /// 目标模式；XML 中 `TargetMode="External"` 为 [`TargetMode::External`]，
    /// 缺省或其他值按 [`TargetMode::Internal`] 处理。
    pub target_mode: TargetMode,
}

/// 一个 part 的 .rels 集合（顺序保持原文件）。
///
/// # 失败情况
///
/// [`Relationships::parse`] 在 XML 非法或 `<Relationship>` 缺少
/// `Id`/`Type`/`Target` 属性时返回 [`OpcError::InvalidRelationships`]。
///
/// # 示例
///
/// ```
/// use docxtpl_opc::{PartUri, Relationships};
///
/// let xml = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
///   <Relationship Id="rId1" Type="http://example.com/rel" Target="styles.xml"/>
///   <Relationship Id="rId2" Type="http://example.com/rel" Target="https://example.com" TargetMode="External"/>
/// </Relationships>"#;
/// let rels = Relationships::parse(xml)?;
/// assert_eq!(rels.len(), 2);
/// let owner = PartUri::new("word/document.xml")?;
/// // target 相对 owner 所在目录规范化
/// assert_eq!(rels.resolve(&owner, "rId1").unwrap().as_str(), "word/styles.xml");
/// // 外部关系不解析
/// assert!(rels.resolve(&owner, "rId2").is_none());
/// # Ok::<(), docxtpl_opc::OpcError>(())
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Relationships(Vec<Relationship>);

impl Relationships {
    /// 解析 .rels XML 文本。
    ///
    /// # 失败情况
    ///
    /// [`OpcError::InvalidRelationships`]：XML 非法，或 `<Relationship>`
    /// 缺少 `Id`/`Type`/`Target` 属性。
    pub fn parse(xml: &str) -> Result<Self, OpcError> {
        Self::parse_in(xml, ".rels")
    }

    /// 解析 .rels；`source` 用于错误信息定位（如 rels 文件路径）。
    pub(crate) fn parse_in(xml: &str, source: &str) -> Result<Self, OpcError> {
        let wrap = |reason: String| OpcError::InvalidRelationships {
            reason: format!("{source}: {reason}"),
        };

        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(true);
        let mut relations = Vec::new();
        loop {
            let event = reader
                .read_event()
                .map_err(|err| wrap(format!("XML 解析失败: {err}")))?;
            let element = match event {
                Event::Start(element) | Event::Empty(element) => element,
                Event::Eof => break,
                _ => continue,
            };
            if element.local_name().as_ref() != "Relationship".as_bytes() {
                continue;
            }
            let id = required_attr(&element, "Id").map_err(wrap)?;
            let rel_type = required_attr(&element, "Type").map_err(wrap)?;
            let target = required_attr(&element, "Target").map_err(wrap)?;
            let target_mode = match attr_value(&element, "TargetMode").map_err(wrap)? {
                Some(mode) if mode == "External" => TargetMode::External,
                Some(_) | None => TargetMode::Internal,
            };
            relations.push(Relationship {
                id,
                rel_type,
                target,
                target_mode,
            });
        }
        Ok(Self(relations))
    }

    /// 按文件顺序迭代全部关系。
    ///
    /// # 示例
    ///
    /// ```
    /// # use docxtpl_opc::Relationships;
    /// let rels = Relationships::parse(
    ///     r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    ///        <Relationship Id="rId1" Type="http://example.com/rel" Target="a"/>
    ///        <Relationship Id="rId2" Type="http://example.com/rel" Target="b"/>
    ///     </Relationships>"#)?;
    /// let ids: Vec<&str> = rels.iter().map(|rel| rel.id.as_str()).collect();
    /// assert_eq!(ids, ["rId1", "rId2"]);
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn iter(&self) -> impl Iterator<Item = &Relationship> {
        self.0.iter()
    }

    /// 按 Id 查找（首个命中）。
    ///
    /// # 示例
    ///
    /// ```
    /// # use docxtpl_opc::Relationships;
    /// # let rels = Relationships::parse(
    /// #     r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    /// #        <Relationship Id="rId1" Type="http://example.com/rel" Target="a"/>
    /// #     </Relationships>"#)?;
    /// assert_eq!(rels.get("rId1").unwrap().target, "a");
    /// assert!(rels.get("missing").is_none());
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn get(&self, id: &str) -> Option<&Relationship> {
        self.0.iter().find(|rel| rel.id == id)
    }

    /// 是否没有关系。
    ///
    /// # 示例
    ///
    /// ```
    /// # use docxtpl_opc::Relationships;
    /// let rels = Relationships::parse(
    ///     r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"/>"#)?;
    /// assert!(rels.is_empty());
    /// assert_eq!(rels.len(), 0);
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// 关系数。
    ///
    /// # 示例
    ///
    /// ```
    /// # use docxtpl_opc::Relationships;
    /// # let rels = Relationships::parse(
    /// #     r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    /// #        <Relationship Id="rId1" Type="http://example.com/rel" Target="a"/>
    /// #        <Relationship Id="rId2" Type="http://example.com/rel" Target="b"/>
    /// #     </Relationships>"#)?;
    /// assert_eq!(rels.len(), 2);
    /// assert!(!rels.is_empty());
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// 按 id 解析内部目标。
    ///
    /// `target` 相对 `owner` 所在目录规范化（`.`/空段折叠，`..` 上溯，
    /// 前导 `/` 视为包内绝对路径）；外部关系、目标逃出包根或无法解析为
    /// 合法 URI 时返回 `None`。
    ///
    /// 注意：返回 [`PartUri`] 只代表路径解析成功，不代表对应 part 存在
    /// （存在性由 [`crate::Package::validate`] 检查）。
    ///
    /// # 示例
    ///
    /// ```
    /// # use docxtpl_opc::{PartUri, Relationships};
    /// # let rels = Relationships::parse(
    /// #     r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
    /// #        <Relationship Id="rId1" Type="http://example.com/rel" Target="../media/logo.png"/>
    /// #     </Relationships>"#)?;
    /// let owner = PartUri::new("word/document.xml")?;
    /// assert_eq!(rels.resolve(&owner, "rId1").unwrap().as_str(), "media/logo.png");
    /// assert!(rels.resolve(&owner, "missing").is_none());
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn resolve(&self, owner: &PartUri, id: &str) -> Option<PartUri> {
        let rel = self.get(id)?;
        if rel.target_mode != TargetMode::Internal {
            return None;
        }
        resolve_relative_to(owner.parent().as_ref(), &rel.target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://example.com/styles" Target="styles.xml"/>
<Relationship Id="rId2" Type="http://example.com/link" Target="https://example.com/doc" TargetMode="External"/>
<Relationship Id="rId3" Type="http://example.com/image" Target="../media/logo.png" TargetMode="Internal"/>
</Relationships>"#;

    #[test]
    fn parses_attributes_and_modes() {
        let rels = Relationships::parse(SAMPLE).unwrap();
        assert_eq!(rels.len(), 3);
        assert!(!rels.is_empty());

        let r1 = rels.get("rId1").unwrap();
        assert_eq!(r1.id, "rId1");
        assert_eq!(r1.rel_type, "http://example.com/styles");
        assert_eq!(r1.target, "styles.xml");
        assert_eq!(r1.target_mode, TargetMode::Internal);

        // TargetMode="External" → External；缺省与 Internal 均为 Internal
        assert_eq!(rels.get("rId2").unwrap().target_mode, TargetMode::External);
        assert_eq!(rels.get("rId3").unwrap().target_mode, TargetMode::Internal);

        assert!(rels.get("missing").is_none());

        // 顺序保持原文件
        let ids: Vec<&str> = rels.iter().map(|rel| rel.id.as_str()).collect();
        assert_eq!(ids, ["rId1", "rId2", "rId3"]);
    }

    #[test]
    fn resolves_targets_relative_to_owner_directory() {
        let rels = Relationships::parse(SAMPLE).unwrap();
        let owner = PartUri::new("word/document.xml").unwrap();
        assert_eq!(
            rels.resolve(&owner, "rId1").unwrap().as_str(),
            "word/styles.xml"
        );
        assert_eq!(
            rels.resolve(&owner, "rId3").unwrap().as_str(),
            "media/logo.png"
        );
        // 外部关系不解析
        assert!(rels.resolve(&owner, "rId2").is_none());
        // 未知 Id
        assert!(rels.resolve(&owner, "nope").is_none());
    }

    #[test]
    fn rejects_missing_required_attributes() {
        for bad in [
            r#"<Relationships><Relationship Type="t" Target="a"/></Relationships>"#,
            r#"<Relationships><Relationship Id="1" Target="a"/></Relationships>"#,
            r#"<Relationships><Relationship Id="1" Type="t"/></Relationships>"#,
        ] {
            let err = Relationships::parse(bad).unwrap_err();
            assert!(
                matches!(err, OpcError::InvalidRelationships { .. }),
                "{bad}: {err:?}"
            );
        }
    }

    #[test]
    fn rejects_broken_xml() {
        let err = Relationships::parse("<Relationships></Wrong>").unwrap_err();
        assert!(matches!(err, OpcError::InvalidRelationships { .. }));
    }
}
