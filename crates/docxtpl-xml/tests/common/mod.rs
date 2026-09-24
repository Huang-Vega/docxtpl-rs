//! 测试公共工具：两棵 [`XmlDocument`] 的全树结构相等比较。

use docxtpl_xml::{NodeId, NodeKind, XmlDocument};

/// 递归比较两棵文档（根标签、属性序列、xmlns 声明、子节点序列、文本/注释/PI 值）。
pub fn trees_equal(a: &XmlDocument, b: &XmlDocument) -> Result<(), String> {
    compare_node(a, a.root(), b, b.root(), "root")
}

fn compare_node(
    a: &XmlDocument,
    ia: NodeId,
    b: &XmlDocument,
    ib: NodeId,
    path: &str,
) -> Result<(), String> {
    let ka = a.node_kind(ia);
    let kb = b.node_kind(ib);
    if ka != kb {
        return Err(format!("{path}: 节点种类 {ka:?} != {kb:?}"));
    }
    match ka {
        NodeKind::Element => {
            let ta = a.tag(ia).ok_or_else(|| format!("{path}: 缺少标签"))?;
            let tb = b.tag(ib).ok_or_else(|| format!("{path}: 缺少标签"))?;
            if ta != tb {
                return Err(format!("{path}: 标签 {ta:?} != {tb:?}"));
            }
            if a.attrs(ia) != b.attrs(ib) {
                return Err(format!(
                    "{path}: 属性序列不一致: {:?} != {:?}",
                    a.attrs(ia),
                    b.attrs(ib)
                ));
            }
            if a.ns_decls(ia) != b.ns_decls(ib) {
                return Err(format!(
                    "{path}: xmlns 声明不一致: {:?} != {:?}",
                    a.ns_decls(ia),
                    b.ns_decls(ib)
                ));
            }
        }
        NodeKind::Text | NodeKind::Comment | NodeKind::Pi => {
            let va = a.node_value(ia);
            let vb = b.node_value(ib);
            if va != vb {
                return Err(format!("{path}: {ka:?} 文本不一致: {va:?} != {vb:?}"));
            }
        }
    }
    let ca = a.children(ia);
    let cb = b.children(ib);
    if ca.len() != cb.len() {
        return Err(format!(
            "{path}: 子节点数量不一致: {} != {}",
            ca.len(),
            cb.len()
        ));
    }
    for (i, (xa, xb)) in ca.iter().zip(cb.iter()).enumerate() {
        let label = match a.node_kind(*xa) {
            NodeKind::Element => a
                .tag(*xa)
                .map(|q| format!("{path}/{}", q.local))
                .unwrap_or_else(|| format!("{path}/{i}")),
            _ => format!("{path}/{i}"),
        };
        compare_node(a, *xa, b, *xb, &label)?;
    }
    Ok(())
}
