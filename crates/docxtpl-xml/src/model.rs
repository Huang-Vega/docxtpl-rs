//! 内存树模型：[`XmlDocument`] 与 Vec arena 节点。

use crate::error::XmlError;
use crate::names;

/// document.xml 常见命名空间 URI 常量。
pub mod ns_uri {
    /// `w`：WordprocessingML 主命名空间。
    pub const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
    /// `wpc`：绘图画布。
    pub const WPC: &str = "http://schemas.microsoft.com/office/word/2010/wordprocessingCanvas";
    /// `mc`：标记兼容性。
    pub const MC: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";
    /// `o`：Office 旧版命名空间。
    pub const O: &str = "urn:schemas-microsoft-com:office:office";
    /// `r`：关系（relationships）命名空间。
    pub const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    /// `m`：OMML 数学公式。
    pub const M: &str = "http://schemas.openxmlformats.org/officeDocument/2006/math";
    /// `v`：VML。
    pub const V: &str = "urn:schemas-microsoft-com:vml";
    /// `wp14`：Word 2010 绘图扩展。
    pub const WP14: &str = "http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing";
    /// `wp`：DrawingML 文字处理绘图。
    pub const WP: &str = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing";
    /// `w10`：旧版 Word 扩展。
    pub const W10: &str = "urn:schemas-microsoft-com:office:word";
    /// `w14`：Word 2010 扩展。
    pub const W14: &str = "http://schemas.microsoft.com/office/word/2010/wordml";
    /// `wpg`：绘图组。
    pub const WPG: &str = "http://schemas.microsoft.com/office/word/2010/wordprocessingGroup";
    /// `wpi`：墨迹。
    pub const WPI: &str = "http://schemas.microsoft.com/office/word/2010/wordprocessingInk";
    /// `wne`：Word 2006 新特性。
    pub const WNE: &str = "http://schemas.openxmlformats.org/word/2006/wordml";
    /// `wps`：形状。
    pub const WPS: &str = "http://schemas.microsoft.com/office/word/2010/wordprocessingShape";
    /// `mo`：Mac Office 扩展。
    pub const MO: &str = "http://schemas.microsoft.com/office/mac/office/2008/main";
    /// `mv`：Mac VML。
    pub const MV: &str = "urn:schemas-microsoft-com:mac:vml";
    /// `pic`：DrawingML 图片。
    pub const PIC: &str = "http://schemas.openxmlformats.org/drawingml/2006/picture";
    /// `a`：DrawingML 主命名空间。
    pub const A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
    /// 隐式 `xml` 前缀绑定的命名空间。
    pub const XML: &str = "http://www.w3.org/XML/1998/namespace";
    /// `xmlns` 保留命名空间。
    pub const XMLNS: &str = "http://www.w3.org/2000/xmlns/";
}

/// arena 节点标识。
///
/// 在文档存活期内稳定；[`XmlDocument::detach`] 不会压缩 arena，
/// 已取得的标识仍然有效。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct NodeId(pub(crate) u32);

/// 限定名：命名空间 URI + 本地名。`ns` 为空串表示无命名空间。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct QName {
    /// 命名空间 URI；空串表示无命名空间。
    pub ns: String,
    /// 本地名（不含前缀）。
    pub local: String,
}

impl QName {
    /// 构造一个限定名。
    pub(crate) fn new(ns: impl Into<String>, local: impl Into<String>) -> Self {
        Self {
            ns: ns.into(),
            local: local.into(),
        }
    }
}

/// 节点种类。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NodeKind {
    /// 元素节点。
    Element,
    /// 文本节点（连续字符数据已合并）。
    Text,
    /// 注释节点。
    Comment,
    /// 处理指令节点。
    Pi,
}

/// XML 解析安全限额。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct XmlLimits {
    /// 最大元素嵌套深度（根元素深度为 1）。
    pub max_depth: usize,
}

impl Default for XmlLimits {
    fn default() -> Self {
        Self { max_depth: 512 }
    }
}

/// 解析过程中采集的元素开标签信息。
pub(crate) struct ElementHead {
    /// 词法前缀（空串表示无前缀）。
    pub prefix: String,
    /// 解析后的限定名。
    pub qname: QName,
    /// 普通属性（输入顺序），值为解码后的字符数据。
    pub attrs: Vec<(QName, String)>,
    /// 与 `attrs` 平行的属性词法前缀（空串无前缀）。
    pub attr_prefix: Vec<String>,
    /// 该元素开标签上自带的 xmlns 声明，(前缀, URI)；空前缀为默认命名空间。
    pub nsdecls: Vec<(String, String)>,
}

/// 单个 arena 节点。
#[derive(Debug)]
pub(crate) struct Node {
    pub(crate) kind: NodeKind,
    pub(crate) parent: Option<NodeId>,
    pub(crate) children: Vec<NodeId>,
    pub(crate) qname: Option<QName>,
    pub(crate) prefix: String,
    pub(crate) attrs: Vec<(QName, String)>,
    pub(crate) attr_prefix: Vec<String>,
    pub(crate) nsdecls: Vec<(String, String)>,
    pub(crate) value: String,
    pub(crate) pi_target: String,
}

impl Node {
    fn new(kind: NodeKind) -> Self {
        Self {
            kind,
            parent: None,
            children: Vec::new(),
            qname: None,
            prefix: String::new(),
            attrs: Vec::new(),
            attr_prefix: Vec::new(),
            nsdecls: Vec::new(),
            value: String::new(),
            pi_target: String::new(),
        }
    }
}

/// 一棵可编辑的 XML 文档树（Vec arena 存储）。
#[derive(Debug)]
pub struct XmlDocument {
    pub(crate) nodes: Vec<Node>,
    pub(crate) root: u32,
    pub(crate) has_decl: bool,
    /// 文档级前缀→URI 总表（首次出现顺序）。
    pub(crate) prefix_uris: Vec<(String, String)>,
}

impl XmlDocument {
    /// 分配一个新节点，返回其标识。
    pub(crate) fn alloc(&mut self, kind: NodeKind) -> NodeId {
        let id = u32::try_from(self.nodes.len()).unwrap_or(u32::MAX);
        self.nodes.push(Node::new(kind));
        NodeId(id)
    }

    /// 依据解析好的开标签信息创建元素节点（尚未挂到父节点）。
    pub(crate) fn create_element(&mut self, head: ElementHead) -> NodeId {
        for (prefix, uri) in &head.nsdecls {
            if !self.prefix_uris.iter().any(|(p, _)| p == prefix) {
                self.prefix_uris.push((prefix.clone(), uri.clone()));
            }
        }
        let id = self.alloc(NodeKind::Element);
        let node = &mut self.nodes[id.0 as usize];
        node.qname = Some(head.qname);
        node.prefix = head.prefix;
        node.attrs = head.attrs;
        node.attr_prefix = head.attr_prefix;
        node.nsdecls = head.nsdecls;
        id
    }

    /// 追加原始字符数据到 `top`；若末个子节点已是文本则与之合并。
    pub(crate) fn push_text(&mut self, top: NodeId, text: &str) {
        if text.is_empty() {
            return;
        }
        let last = self.nodes[top.0 as usize].children.last().copied();
        if let Some(last_id) = last {
            if self.nodes[last_id.0 as usize].kind == NodeKind::Text {
                self.nodes[last_id.0 as usize].value.push_str(text);
                return;
            }
        }
        let id = self.alloc(NodeKind::Text);
        self.nodes[id.0 as usize].value.push_str(text);
        self.attach(top, id);
    }

    /// 把已存在节点挂到父节点末尾。
    pub(crate) fn attach(&mut self, parent: NodeId, child: NodeId) {
        if let Some(old) = self.nodes[child.0 as usize].parent {
            self.nodes[old.0 as usize].children.retain(|&c| c != child);
        }
        self.nodes[child.0 as usize].parent = Some(parent);
        self.nodes[parent.0 as usize].children.push(child);
    }

    /// 解析命名空间前缀：先查本元素自带声明，再沿父链向上。
    pub(crate) fn resolve_prefix(
        &self,
        node: Option<NodeId>,
        local_decls: &[(String, String)],
        prefix: &str,
    ) -> Option<String> {
        if prefix == "xml" {
            return Some(ns_uri::XML.to_string());
        }
        if let Some(uri) = local_decls
            .iter()
            .rev()
            .find(|(p, _)| p == prefix)
            .map(|(_, u)| u)
        {
            return Some(uri.clone());
        }
        let mut cur = node;
        while let Some(id) = cur {
            let n = &self.nodes[id.0 as usize];
            if let Some((_, uri)) = n.nsdecls.iter().rev().find(|(p, _)| p == prefix) {
                return Some(uri.clone());
            }
            cur = n.parent;
        }
        None
    }

    /// 依据原始 (属性名, 值) 序列构建元素开标签信息。
    ///
    /// `strict` 为真时，未绑定前缀返回错误；宽松模式下未绑定前缀的
    /// 名称按无命名空间处理但保留词法前缀（对齐 libxml2 recover）。
    pub(crate) fn build_head(
        &self,
        parent: Option<NodeId>,
        raw_name: &str,
        raw_attrs: Vec<(String, String)>,
        strict: bool,
        pos_for_error: usize,
        input: &str,
    ) -> Result<ElementHead, XmlError> {
        let mut nsdecls: Vec<(String, String)> = Vec::new();
        let mut attrs: Vec<(QName, String)> = Vec::new();
        let mut attr_prefix: Vec<String> = Vec::new();

        for (raw, value) in &raw_attrs {
            if let Some(rest) = raw.strip_prefix("xmlns:") {
                nsdecls.push((rest.to_string(), value.clone()));
            } else if raw == "xmlns" {
                nsdecls.push((String::new(), value.clone()));
            }
        }

        let (prefix, local) = split_qname(raw_name);
        let qname = self.resolve_qname(
            parent,
            &nsdecls,
            prefix,
            local,
            strict,
            input,
            pos_for_error,
        )?;

        if strict {
            for (raw, _) in &raw_attrs {
                if raw_attrs.iter().filter(|(n, _)| n == raw).count() > 1 {
                    return Err(XmlError::at(
                        input,
                        pos_for_error,
                        format!("元素 {raw_name} 存在重复属性 {raw}"),
                    ));
                }
            }
        }

        for (raw, value) in raw_attrs {
            if raw == "xmlns" || raw.starts_with("xmlns:") {
                continue;
            }
            let (aprefix, alocal) = split_qname(&raw);
            let aqname = if aprefix.is_empty() {
                QName::new("", alocal)
            } else {
                self.resolve_qname(
                    parent,
                    &nsdecls,
                    aprefix,
                    alocal,
                    strict,
                    input,
                    pos_for_error,
                )?
            };
            attrs.push((aqname, value));
            attr_prefix.push(aprefix.to_string());
        }

        Ok(ElementHead {
            prefix: prefix.to_string(),
            qname,
            attrs,
            attr_prefix,
            nsdecls,
        })
    }

    /// 解析单个 QName 前缀。
    #[allow(clippy::too_many_arguments)]
    fn resolve_qname(
        &self,
        parent: Option<NodeId>,
        local_decls: &[(String, String)],
        prefix: &str,
        local: &str,
        strict: bool,
        input: &str,
        pos: usize,
    ) -> Result<QName, XmlError> {
        if prefix.is_empty() {
            let ns = self
                .resolve_prefix(parent, local_decls, "")
                .unwrap_or_default();
            return Ok(QName::new(ns, local));
        }
        match self.resolve_prefix(parent, local_decls, prefix) {
            Some(uri) => Ok(QName::new(uri, local)),
            None if strict => Err(XmlError::at(
                input,
                pos,
                format!("未绑定的命名空间前缀 {prefix}:{local}"),
            )),
            None => Ok(QName::new("", local)),
        }
    }

    /// 返回文档根元素标识。
    pub fn root(&self) -> NodeId {
        NodeId(self.root)
    }

    /// 返回元素的限定名；非元素节点返回 `None`。
    pub fn tag(&self, id: NodeId) -> Option<&QName> {
        self.nodes[id.0 as usize].qname.as_ref()
    }

    /// 返回直接子节点标识序列（文档序）。
    pub fn children(&self, id: NodeId) -> &[NodeId] {
        &self.nodes[id.0 as usize].children
    }

    /// 返回父节点标识；根节点或已脱离节点返回 `None`。
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id.0 as usize].parent
    }

    /// 返回节点自身及其全部后代（深度优先、文档序）。
    pub fn descendants(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![id];
        while let Some(cur) = stack.pop() {
            out.push(cur);
            let kids = self.nodes[cur.0 as usize].children.clone();
            for k in kids.into_iter().rev() {
                stack.push(k);
            }
        }
        out
    }

    /// 返回节点种类。
    pub fn node_kind(&self, id: NodeId) -> NodeKind {
        self.nodes[id.0 as usize].kind
    }

    /// 返回节点值：
    /// 文本节点为字符数据；注释为注释正文；PI 为目标之后的数据。
    /// 元素节点返回空串。
    pub fn node_value(&self, id: NodeId) -> &str {
        &self.nodes[id.0 as usize].value
    }

    /// 返回元素某属性的值（按命名空间 URI 与本地名匹配）。
    pub fn attr(&self, id: NodeId, ns: &str, local: &str) -> Option<&str> {
        self.nodes[id.0 as usize]
            .attrs
            .iter()
            .find(|(q, _)| q.ns == ns && q.local == local)
            .map(|(_, v)| v.as_str())
    }

    /// 返回元素全部属性（输入顺序）。
    pub fn attrs(&self, id: NodeId) -> &[(QName, String)] {
        &self.nodes[id.0 as usize].attrs
    }

    /// 返回元素开标签上自带的 xmlns 声明（输入顺序）：
    /// 元组为 `(前缀, URI)`，空前缀表示默认命名空间声明。
    pub fn ns_decls(&self, id: NodeId) -> &[(String, String)] {
        &self.nodes[id.0 as usize].nsdecls
    }

    /// 文档级前缀→URI 查询（供创建新元素时校验）。
    pub fn prefix_uri(&self, prefix: &str) -> Option<&str> {
        self.prefix_uris
            .iter()
            .find(|(p, _)| p == prefix)
            .map(|(_, u)| u.as_str())
    }

    /// 设置属性值；属性已存在则原位替换（保持顺序），否则追加到末尾。
    ///
    /// `ns` 为空串表示无命名空间属性；否则按文档级 URI→前缀映射
    /// 选择序列化前缀（fix_tables 只写 `w:` 属性）。
    pub fn set_attr(&mut self, id: NodeId, ns: &str, local: &str, value: String) {
        let node = &mut self.nodes[id.0 as usize];
        if let Some(slot) = node
            .attrs
            .iter_mut()
            .find(|(q, _)| q.ns == ns && q.local == local)
        {
            slot.1 = value;
            return;
        }
        let prefix = if ns.is_empty() {
            String::new()
        } else {
            self.prefix_uris
                .iter()
                .find(|(_, u)| u == ns)
                .map(|(p, _)| p.clone())
                .unwrap_or_default()
        };
        node.attrs.push((QName::new(ns, local), value));
        node.attr_prefix.push(prefix);
    }

    /// 创建一个 `w:` 前缀的游离元素（未挂载）。
    ///
    /// 属性列表为 `(本地名, 值)`；`w` 前缀的 URI 取文档内已有的
    /// `xmlns:w` 声明，文档未声明时返回错误。
    pub fn new_w_element(
        &mut self,
        local: &str,
        attrs: Vec<(String, String)>,
    ) -> Result<NodeId, XmlError> {
        self.new_prefixed_element("w", ns_uri::W, local, attrs)
    }

    /// 创建任意已声明前缀的游离元素（未挂载）。
    ///
    /// `prefix` 必须在文档中声明且其 URI 等于 `expected_uri`；
    /// 属性列表为 `(本地名, 值)`，属性与元素同前缀。
    pub fn new_prefixed_element(
        &mut self,
        prefix: &str,
        expected_uri: &str,
        local: &str,
        attrs: Vec<(String, String)>,
    ) -> Result<NodeId, XmlError> {
        if !names::is_name_start(local.chars().next().unwrap_or('\u{0}'))
            || local.chars().skip(1).any(|c| !names::is_name_char(c))
        {
            return Err(XmlError::Parse {
                line: 0,
                col: 0,
                offset: 0,
                message: format!("非法元素本地名 {local:?}"),
            });
        }
        match self.prefix_uri(prefix) {
            Some(uri) if uri == expected_uri => {}
            _ => {
                return Err(XmlError::Parse {
                    line: 0,
                    col: 0,
                    offset: 0,
                    message: format!(
                        "文档内未声明前缀 {prefix:?}（{expected_uri}），无法创建该前缀元素"
                    ),
                });
            }
        }
        let mut pairs = Vec::with_capacity(attrs.len());
        let mut prefixes = Vec::with_capacity(attrs.len());
        for (alocal, value) in attrs {
            pairs.push((QName::new(expected_uri, alocal), value));
            prefixes.push(prefix.to_string());
        }
        let id = self.alloc(NodeKind::Element);
        let node = &mut self.nodes[id.0 as usize];
        node.qname = Some(QName::new(expected_uri, local));
        node.prefix = prefix.to_string();
        node.attrs = pairs;
        node.attr_prefix = prefixes;
        Ok(id)
    }

    /// 用单个文本节点替换元素的全部子节点（对齐 python-docx 的 `element.text = ...`：
    /// 空串会保留显式文本节点，序列化为 `<x></x>` 而非 `<x/>`）。
    pub fn set_element_text(&mut self, id: NodeId, text: &str) {
        for child in std::mem::take(&mut self.nodes[id.0 as usize].children) {
            self.nodes[child.0 as usize].parent = None;
        }
        let text_id = self.alloc(NodeKind::Text);
        self.nodes[text_id.0 as usize].value = text.to_string();
        self.attach(id, text_id);
    }

    /// 元素的直接文本（首个文本子节点的值；无文本子节点返回 None）。
    #[must_use]
    pub fn element_text(&self, id: NodeId) -> Option<&str> {
        self.nodes[id.0 as usize]
            .children
            .iter()
            .find(|c| self.nodes[c.0 as usize].kind == NodeKind::Text)
            .map(|c| self.nodes[c.0 as usize].value.as_str())
    }

    /// 把 `child` 追加为 `parent` 的最后一个子节点；
    /// 若 `child` 已有父节点则先从原位置脱离。
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) {
        self.attach(parent, child);
    }

    /// 从父节点移除该节点（节点本身保留在 arena 中，可重新挂载）。
    pub fn detach(&mut self, id: NodeId) {
        if let Some(parent) = self.nodes[id.0 as usize].parent.take() {
            self.nodes[parent.0 as usize].children.retain(|&c| c != id);
        }
    }
}

/// 拆分词法限定名 `prefix:local`；冒号多于一个时按首个切分（宽松容错）。
pub(crate) fn split_qname(raw: &str) -> (&str, &str) {
    match raw.split_once(':') {
        Some((prefix, local)) => (prefix, local),
        None => ("", raw),
    }
}
