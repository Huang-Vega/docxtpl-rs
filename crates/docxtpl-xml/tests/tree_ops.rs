//! 树操作测试：set_attr / new_w_element / append_child / detach
//! （模拟 fix_tables 对 tblPr/tr/tc 的改写）。

use docxtpl_xml::{ns_uri, NodeId, NodeKind, QName, XmlDocument, XmlLimits};

const DOC: &str = r#"<w:root xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:tbl><w:tr><w:tc><w:p/></w:tc></w:tr></w:tbl></w:root>"#;

fn q(ns: &str, local: &str) -> QName {
    QName {
        ns: ns.to_string(),
        local: local.to_string(),
    }
}

fn parse() -> XmlDocument {
    XmlDocument::parse_strict(DOC, &XmlLimits::default()).expect("严格解析失败")
}

fn is_w(doc: &XmlDocument, id: NodeId, local: &str) -> bool {
    doc.tag(id) == Some(&q(ns_uri::W, local))
}

#[test]
fn read_navigation() {
    let doc = parse();
    let root = doc.root();
    assert_eq!(doc.tag(root), Some(&q(ns_uri::W, "root")));
    let tbl = doc.children(root)[0];
    assert!(is_w(&doc, tbl, "tbl"));
    assert_eq!(doc.parent(tbl), Some(root));
    let tr = doc.children(tbl)[0];
    let tc = doc.children(tr)[0];
    let p = doc.children(tc)[0];
    assert!(is_w(&doc, p, "p"));
    // descendants 文档序且含自身。
    let desc = doc.descendants(tbl);
    let locals: Vec<&str> = desc
        .iter()
        .filter_map(|id| doc.tag(*id).map(|qn| qn.local.as_str()))
        .collect();
    assert_eq!(locals, vec!["tbl", "tr", "tc", "p"]);
    assert_eq!(doc.node_kind(p), NodeKind::Element);
    assert_eq!(doc.node_value(p), "");
}

#[test]
fn set_attr_new_and_replace_keeps_order() {
    let mut doc = parse();
    let root = doc.root();
    let tbl = doc.children(root)[0];
    doc.set_attr(tbl, ns_uri::W, "fake1", "1".to_string());
    doc.set_attr(tbl, ns_uri::W, "fake2", "2".to_string());
    doc.set_attr(tbl, ns_uri::W, "fake1", "x".to_string());
    assert_eq!(doc.attr(tbl, ns_uri::W, "fake1"), Some("x"));
    let names: Vec<&str> = doc
        .attrs(tbl)
        .iter()
        .map(|(qn, _)| qn.local.as_str())
        .collect();
    assert_eq!(names, vec!["fake1", "fake2"]);
    let ser = doc.serialize();
    assert!(ser.contains("w:fake1=\"x\""), "序列化缺 fake1: {ser}");
    assert!(ser.contains("w:fake2=\"2\""), "序列化缺 fake2: {ser}");
}

#[test]
fn new_w_element_and_append() {
    let mut doc = parse();
    let root = doc.root();
    let tbl = doc.children(root)[0];
    let tr = doc.children(tbl)[0];
    let tc = doc.children(tr)[0];

    // 模拟 fix_tables：给 tc 补 tcPr 风格的新元素。
    let tcpr = doc
        .new_w_element("tcPr", vec![("added".to_string(), "1".to_string())])
        .expect("创建 w:tcPr");
    assert_eq!(doc.tag(tcpr), Some(&q(ns_uri::W, "tcPr")));
    assert_eq!(doc.attr(tcpr, ns_uri::W, "added"), Some("1"));
    // 游离节点暂无父。
    assert_eq!(doc.parent(tcpr), None);
    doc.append_child(tc, tcpr);
    assert_eq!(doc.parent(tcpr), Some(tc));
    assert_eq!(doc.children(tc).len(), 2);
    assert_eq!(doc.children(tc)[1], tcpr);

    let ser = doc.serialize();
    assert!(ser.contains("<w:tcPr w:added=\"1\"/>"), "序列化: {ser}");
}

#[test]
fn detach_removes_and_can_reinsert() {
    let mut doc = parse();
    let root = doc.root();
    let tbl = doc.children(root)[0];
    let tr = doc.children(tbl)[0];
    let tc = doc.children(tr)[0];
    let p = doc.children(tc)[0];

    doc.detach(p);
    assert_eq!(doc.parent(p), None);
    assert!(doc.children(tc).is_empty());
    // 重新挂回去。
    doc.append_child(tc, p);
    assert_eq!(doc.children(tc), vec![p]);

    // 直接移动：append_child 自动脱离旧父。
    let new_tc = doc.new_w_element("tc", vec![]).expect("创建 w:tc");
    doc.append_child(tr, new_tc);
    doc.append_child(new_tc, p);
    assert_eq!(doc.children(tc).len(), 0);
    assert_eq!(doc.children(new_tc), vec![p]);
    assert_eq!(doc.parent(p), Some(new_tc));
}

#[test]
fn new_w_element_without_w_namespace_errors() {
    let mut doc = XmlDocument::parse_strict("<a/>", &XmlLimits::default()).unwrap();
    let err = doc.new_w_element("p", vec![]);
    assert!(err.is_err(), "无 w 声明时创建 w: 元素应失败");
}

#[test]
fn rewritten_tree_serializes_strictly() {
    let doc = parse();
    let ser1 = doc.serialize();
    let reparsed = XmlDocument::parse_strict(&ser1, &XmlLimits::default())
        .expect("改写树序列化结果必须仍可严格解析");
    assert_eq!(reparsed.serialize(), ser1);
}
