//! docxtpl-xml：XML 可编辑片段层（运行时零依赖）。
//!
//! 提供两种解析策略（见 ADR-002、ADR-004）：
//! - [`XmlDocument::parse_strict`]：标准良构 XML 子集，零恢复，用于校验
//!   python-docx 产出的原始 part；
//! - [`XmlDocument::parse_lenient`]：模拟 libxml2 `recover` 在渲染语料上的
//!   愈合规则，用于字符串管线渲染后的 XML；每个愈合动作记录一条
//!   [`Recovery`] 诊断。超深度、DTD/外部实体等安全错误在两种模式下都报错。
//!
//! 树模型为 Vec arena（节点带父索引），元素记录输入顺序的属性、原始前缀
//! 以及开标签上自带的 xmlns 声明；连续字符数据合并为单个文本节点。
//! 序列化对齐 lxml 风格（空元素自闭合、属性双引号、始终转义 `>`），
//! 不追求与 lxml 字节相同（差异由 c14n 归一），但树结构一致。

mod error;
mod input;
mod lenient;
mod model;
mod names;
mod serialize;
mod strict;

pub use error::XmlError;
pub use model::ns_uri;
pub use model::{NodeId, NodeKind, QName, XmlDocument, XmlLimits};

/// 宽松解析结果：愈合后的文档与全部恢复诊断。
#[derive(Debug)]
pub struct ParseOutcome {
    /// 解析得到的文档树。
    pub doc: XmlDocument,
    /// 解析过程中发生的全部恢复动作（按发生顺序）。
    pub diagnostics: Vec<Recovery>,
}

/// 一次恢复动作的诊断信息。
#[derive(Clone, Debug)]
pub struct Recovery {
    /// 恢复类别。
    pub kind: RecoveryKind,
    /// 触发位置的字节偏移（从输入起始计）。
    pub offset: usize,
    /// 1 起始行号。
    pub line: usize,
    /// 1 起始列号。
    pub col: usize,
    /// 人类可读详情。
    pub detail: String,
}

/// 恢复动作类别（与 libxml2 recover 的探针实证规则一一对应）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryKind {
    /// 非法实体引用被丢弃（裸 `&`、未定义实体、畸形数字引用）。
    BadEntityDropped,
    /// 未闭合元素被自动闭合（EOF、祖先先闭合、标签内遇 `<`）。
    TagAutoClosed,
    /// 不在祖先栈中的结束标签，被当作栈顶元素的闭合。
    StrayEndTag,
    /// 畸形标签（孤立 `<`、畸形属性、未闭合注释/PI 等）。
    MalformedTag,
    /// 根元素前后的杂散内容被丢弃。
    PrologTailDropped,
    /// 未归入上述类别的其他恢复。
    Other,
}

impl XmlDocument {
    /// 严格解析：要求输入良构，任何语法/实体/安全问题都返回 [`XmlError`]。
    pub fn parse_strict(xml: &str, limits: &XmlLimits) -> Result<XmlDocument, XmlError> {
        strict::parse(xml, limits)
    }

    /// 宽松解析：按 libxml2 recover 规则愈合结构性损坏；
    /// 超深度、DTD/外部实体等安全错误仍返回 [`XmlError`]。
    pub fn parse_lenient(xml: &str, limits: &XmlLimits) -> Result<ParseOutcome, XmlError> {
        lenient::parse(xml, limits)
    }

    /// 按 lxml 风格序列化整棵文档（裁剪元素上与祖先重复的 xmlns 声明，
    /// 对应正文换挂后的 lxml 行为）。
    #[must_use]
    pub fn serialize(&self) -> String {
        serialize::serialize(self)
    }

    /// 页眉/页脚专用序列化：保留元素开标签词法自带的 xmlns 声明（即使与
    /// 祖先重复），对齐 story part 经 `XmlPart.load` 独立解析、无换挂的
    /// lxml 输出（ADR-006）。
    #[must_use]
    pub fn serialize_story(&self) -> String {
        serialize::serialize_with(
            self,
            serialize::SerializeOptions {
                retain_redundant_ns: true,
            },
        )
    }

    /// 删除仅由 XML 空白字符组成、且不在 `xml:space="preserve"` 作用域内
    /// 的文本节点。
    ///
    /// 对齐 python-docx oxml 解析器的 `remove_blank_text=True`：页眉/页脚
    /// 映射为新 `XmlPart` 时按该选项解析，注入图片 XML 里 python-docx
    /// 模板自带的换行/缩进空白会被剥除（ADR-006）；但 `w:t` 等元素显式
    /// 标注 `xml:space="preserve"` 时，其中的空白文本必须保留（libxml2
    /// 对该解析选项的语义：沿祖先轴追踪 xml:space，`preserve` 保留、
    /// `default` 恢复裁剪）。
    pub fn strip_blank_text(&mut self) {
        let root = self.root();
        let mut blank = Vec::new();
        let mut stack: Vec<(NodeId, bool)> = vec![(root, false)];
        while let Some((id, preserved)) = stack.pop() {
            let mut preserved = preserved;
            if self.node_kind(id) == NodeKind::Element {
                if let Some(mode) = self.attr(id, ns_uri::XML, "space") {
                    preserved = mode == "preserve";
                }
            } else if self.node_kind(id) == NodeKind::Text
                && !preserved
                && !self.node_value(id).is_empty()
                && self
                    .node_value(id)
                    .chars()
                    .all(|c| matches!(c, ' ' | '\t' | '\r' | '\n'))
            {
                blank.push(id);
            }
            // 栈后进先出，反转子序以文档顺序处理（顺序对结果无影响，仅便于调试）。
            for child in self.children(id).iter().rev() {
                stack.push((*child, preserved));
            }
        }
        for id in blank {
            self.detach(id);
        }
    }
}
