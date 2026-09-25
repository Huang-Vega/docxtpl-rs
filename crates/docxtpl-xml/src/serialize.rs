//! 保留式序列化器，输出风格逐字节对齐 lxml `tostring`（oracle 差分同时
//! 比较原始 part 字节与 c14n，故声明引号/换行等细节也须一致）。

use crate::model::ns_uri;
use crate::model::{NodeId, NodeKind, XmlDocument};

/// 序列化开关。
#[derive(Clone, Copy, Debug)]
pub(crate) struct SerializeOptions {
    /// 是否保留元素开标签上词法自带、但与祖先重复的 xmlns 声明。
    ///
    /// 正文渲染时 lxml 把渲染树**换挂**到原始 document 元素下，序列化会
    /// 剥掉这类冗余声明（默认 `false` 的裁剪行为）；页眉/页脚则是
    /// `XmlPart.load` 独立解析的新树（无换挂），注入图片 `wp:inline` 上
    /// 的 `xmlns:wp/xmlns:r` 等冗余声明必须原样保留（ADR-006）。
    pub retain_redundant_ns: bool,
}

/// 序列化整棵文档；有 XML 声明时输出 lxml 风格的单引号声明，
/// 声明后带一个换行（与 python-docx 落盘的 document.xml 一致）。
pub(crate) fn serialize(doc: &XmlDocument) -> String {
    serialize_with(
        doc,
        SerializeOptions {
            retain_redundant_ns: false,
        },
    )
}

/// 带选项的序列化（页眉/页脚使用，见 [`SerializeOptions`]）。
pub(crate) fn serialize_with(doc: &XmlDocument, options: SerializeOptions) -> String {
    let mut out = String::new();
    if doc.has_decl {
        out.push_str("<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n");
    }
    serialize_node(doc, doc.root(), options, &mut out);
    out
}

/// 序列化以 `id` 为根的子树（无 XML 声明）。
///
/// 供 Subdoc 片段输出使用：上游 `etree.tostring(body, encoding="unicode")`
/// 后用正则剥掉 body 开/闭标签，等价于逐个序列化 body 的直接子节点——
/// 片段不携带任何命名空间声明（body 开标签上被提升的声明随标签一起剥掉），
/// 前缀绑定由主文档提供。
pub(crate) fn serialize_subtree(
    doc: &XmlDocument,
    id: NodeId,
    options: SerializeOptions,
) -> String {
    let mut out = String::new();
    serialize_node(doc, id, options, &mut out);
    out
}

fn serialize_node(doc: &XmlDocument, id: NodeId, options: SerializeOptions, out: &mut String) {
    let node = &doc.nodes[id.0 as usize];
    match node.kind {
        NodeKind::Text => {
            escape_text(&node.value, out);
        }
        NodeKind::Comment => {
            out.push_str("<!--");
            out.push_str(&node.value);
            out.push_str("-->");
        }
        NodeKind::Pi => {
            out.push_str("<?");
            out.push_str(&node.pi_target);
            if !node.value.is_empty() {
                out.push(' ');
                out.push_str(&node.value);
            }
            out.push_str("?>");
        }
        NodeKind::Element => serialize_element(doc, id, options, out),
    }
}

fn serialize_element(doc: &XmlDocument, id: NodeId, options: SerializeOptions, out: &mut String) {
    let node = &doc.nodes[id.0 as usize];
    out.push('<');
    let prefix = &node.prefix;
    let local = node
        .qname
        .as_ref()
        .map(|q| q.local.as_str())
        .unwrap_or_default();
    if prefix.is_empty() {
        out.push_str(local);
    } else {
        out.push_str(prefix);
        out.push(':');
        out.push_str(local);
    }
    for (prefix, uri) in &node.nsdecls {
        // lxml 语义：祖先轴上已有同前缀同 URI 的绑定时，本元素输出省略
        // 该声明（冗余裁剪，见 ADR-005 §2）；其余声明保持原有顺序。
        if !options.retain_redundant_ns {
            let parent = node.parent;
            if doc
                .ancestor_ns_binding(parent, prefix)
                .is_some_and(|u| u == uri)
            {
                continue;
            }
        }
        out.push(' ');
        if prefix.is_empty() {
            out.push_str("xmlns=\"");
        } else {
            out.push_str("xmlns:");
            out.push_str(prefix);
            out.push_str("=\"");
        }
        escape_attr(uri, out);
        out.push('"');
    }
    for (i, (qname, value)) in node.attrs.iter().enumerate() {
        out.push(' ');
        // 隐式 xml 前缀；其余前缀走解析时记录的词法前缀。
        let stored = node.attr_prefix.get(i).cloned().unwrap_or_default();
        if !stored.is_empty() {
            out.push_str(&stored);
            out.push(':');
        } else if qname.ns == ns_uri::XML {
            out.push_str("xml:");
        }
        out.push_str(&qname.local);
        out.push_str("=\"");
        escape_attr(value, out);
        out.push('"');
    }
    if node.children.is_empty() {
        out.push('/');
        out.push('>');
        return;
    }
    out.push('>');
    // 先拷贝子节点序列，避免递归借用冲突。
    let children = node.children.clone();
    for child in children {
        serialize_node(doc, child, options, out);
    }
    out.push_str("</");
    if !prefix.is_empty() {
        out.push_str(prefix);
        out.push(':');
    }
    out.push_str(local);
    out.push('>');
}

/// 文本转义：`& < >`，字面回车写成 `&#13;`；引号不转义。
fn escape_text(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\r' => out.push_str("&#13;"),
            other => out.push(other),
        }
    }
}

/// 属性值转义：`& < > "`，制表/换行/回车写成字符引用。
fn escape_attr(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' => out.push_str("&#9;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            other => out.push(other),
        }
    }
}
