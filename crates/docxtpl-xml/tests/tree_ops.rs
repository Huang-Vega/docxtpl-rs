//! Tree operation tests: set_attr / new_w_element / append_child / detach
//! (simulating the fix_tables rewrite of tblPr/tr/tc).

use docxtpl_xml::{ns_uri, NodeId, NodeKind, QName, XmlDocument, XmlLimits};

const DOC: &str = r#"<w:root xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:tbl><w:tr><w:tc><w:p/></w:tc></w:tr></w:tbl></w:root>"#;

fn q(ns: &str, local: &str) -> QName {
    QName {
        ns: ns.to_string(),
        local: local.to_string(),
    }
}

fn parse() -> XmlDocument {
    XmlDocument::parse_strict(DOC, &XmlLimits::default()).expect("strict parsing failed")
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
    // descendants is in document order and includes the node itself.
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
    assert!(
        ser.contains("w:fake1=\"x\""),
        "serialized output missing fake1: {ser}"
    );
    assert!(
        ser.contains("w:fake2=\"2\""),
        "serialized output missing fake2: {ser}"
    );
}

#[test]
fn new_w_element_and_append() {
    let mut doc = parse();
    let root = doc.root();
    let tbl = doc.children(root)[0];
    let tr = doc.children(tbl)[0];
    let tc = doc.children(tr)[0];

    // Simulate fix_tables: add a new tcPr-style element to tc.
    let tcpr = doc
        .new_w_element("tcPr", vec![("added".to_string(), "1".to_string())])
        .expect("creating w:tcPr");
    assert_eq!(doc.tag(tcpr), Some(&q(ns_uri::W, "tcPr")));
    assert_eq!(doc.attr(tcpr, ns_uri::W, "added"), Some("1"));
    // A detached node has no parent yet.
    assert_eq!(doc.parent(tcpr), None);
    doc.append_child(tc, tcpr);
    assert_eq!(doc.parent(tcpr), Some(tc));
    assert_eq!(doc.children(tc).len(), 2);
    assert_eq!(doc.children(tc)[1], tcpr);

    let ser = doc.serialize();
    assert!(ser.contains("<w:tcPr w:added=\"1\"/>"), "serialized: {ser}");
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
    // Re-attach it.
    doc.append_child(tc, p);
    assert_eq!(doc.children(tc), vec![p]);

    // Direct move: append_child detaches from the old parent automatically.
    let new_tc = doc.new_w_element("tc", vec![]).expect("creating w:tc");
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
    assert!(
        err.is_err(),
        "creating a w: element should fail when w is not declared"
    );
}

#[test]
fn rewritten_tree_serializes_strictly() {
    let doc = parse();
    let ser1 = doc.serialize();
    let reparsed = XmlDocument::parse_strict(&ser1, &XmlLimits::default())
        .expect("serialization of the rewritten tree must still parse strictly");
    assert_eq!(reparsed.serialize(), ser1);
}

#[test]
fn deepcopy_promotes_inherited_namespace_binding() {
    let mut dst =
        XmlDocument::parse_strict(r#"<root xmlns:p="urn:shared"/>"#, &XmlLimits::default())
            .unwrap();
    let src = XmlDocument::parse_strict(
        r#"<outer xmlns:p="urn:shared"><item><p:child p:value="x"/></item></outer>"#,
        &XmlLimits::default(),
    )
    .unwrap();
    let item = src.children(src.root())[0];
    let copy = dst.deepcopy_element(&src, item).unwrap();
    dst.append_child(dst.root(), copy);

    let xml = dst.serialize();
    assert_eq!(
        xml,
        r#"<root xmlns:p="urn:shared"><item><p:child p:value="x"/></item></root>"#
    );
    XmlDocument::parse_strict(&xml, &XmlLimits::default()).unwrap();
}

#[test]
fn deepcopy_renames_inherited_prefix_that_conflicts_in_target() {
    let mut dst =
        XmlDocument::parse_strict(r#"<root xmlns:p="urn:target"/>"#, &XmlLimits::default())
            .unwrap();
    let src = XmlDocument::parse_strict(
        r#"<outer xmlns:p="urn:source"><item><p:child p:value="x"/></item></outer>"#,
        &XmlLimits::default(),
    )
    .unwrap();
    let item = src.children(src.root())[0];
    let copy = dst.deepcopy_element(&src, item).unwrap();
    dst.append_child(dst.root(), copy);

    let xml = dst.serialize();
    assert_eq!(
        xml,
        r#"<root xmlns:p="urn:target"><item xmlns:ns0="urn:source"><ns0:child ns0:value="x"/></item></root>"#
    );
    XmlDocument::parse_strict(&xml, &XmlLimits::default()).unwrap();
}

#[test]
fn deepcopy_generated_prefix_avoids_other_inherited_prefixes() {
    let mut dst =
        XmlDocument::parse_strict(r#"<root xmlns:p="urn:target"/>"#, &XmlLimits::default())
            .unwrap();
    let src = XmlDocument::parse_strict(
        r#"<outer xmlns:p="urn:source" xmlns:ns0="urn:other"><item><p:child/><ns0:other/></item></outer>"#,
        &XmlLimits::default(),
    )
    .unwrap();
    let item = src.children(src.root())[0];
    let copy = dst.deepcopy_element(&src, item).unwrap();
    dst.append_child(dst.root(), copy);

    let xml = dst.serialize();
    assert_eq!(
        xml,
        r#"<root xmlns:p="urn:target"><item xmlns:ns1="urn:source" xmlns:ns0="urn:other"><ns1:child/><ns0:other/></item></root>"#
    );
    XmlDocument::parse_strict(&xml, &XmlLimits::default()).unwrap();
}
