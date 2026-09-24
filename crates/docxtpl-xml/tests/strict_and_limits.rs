//! 严格解析良构性、CR 规范化、深度限额与 DTD 安全策略测试。

use docxtpl_xml::{XmlDocument, XmlError, XmlLimits};

const L: XmlLimits = XmlLimits { max_depth: 512 };

fn strict_err(xml: &str) -> XmlError {
    XmlDocument::parse_strict(xml, &L).expect_err("严格解析本应失败")
}

// ---- 严格模式良构性 ----------------------------------------------------

#[test]
fn strict_rejects_unknown_entity() {
    let e = strict_err("<a>&bogus;</a>");
    assert!(matches!(e, XmlError::Parse { .. }), "实际: {e:?}");
}

#[test]
fn strict_rejects_bare_amp() {
    assert!(matches!(strict_err("<a>a & b</a>"), XmlError::Parse { .. }));
}

#[test]
fn strict_rejects_duplicate_attr() {
    let e = strict_err(r#"<a x="1" x="2"/>"#);
    assert!(matches!(e, XmlError::Parse { .. }), "实际: {e:?}");
}

#[test]
fn strict_rejects_unbound_prefix() {
    let e = strict_err("<x:a/>");
    assert!(matches!(e, XmlError::Parse { .. }), "实际: {e:?}");
}

#[test]
fn strict_rejects_mismatched_tag() {
    let e = strict_err("<a><b></a></b>");
    assert!(matches!(e, XmlError::Parse { .. }), "实际: {e:?}");
}

#[test]
fn strict_rejects_garbage_after_root() {
    let e = strict_err("<a/>tail");
    assert!(matches!(e, XmlError::Parse { .. }), "实际: {e:?}");
}

#[test]
fn strict_rejects_unclosed() {
    // EOF 未闭合既可能报 Incomplete，也可能是带定位的 Parse；二者皆可。
    assert!(matches!(
        strict_err("<a><b>text"),
        XmlError::Incomplete(..) | XmlError::Parse { .. }
    ));
}

#[test]
fn strict_accepts_decl_comments_pi_and_whitespace() {
    let xml = "<?xml version=\"1.0\"?>\n<!-- hi --><?app data?>\n<a>x</a>\n";
    let doc = XmlDocument::parse_strict(xml, &L).expect("应严格通过");
    assert!(doc.serialize().starts_with("<?xml version='1.0'"));
}

// ---- CR 与属性空白规范化 -----------------------------------------------

#[test]
fn literal_cr_normalized_in_text() {
    // 字面 \r 与 \r\n 在解析层归一为 \n。
    let doc = XmlDocument::parse_strict("<a>x\ry\r\nz</a>", &L).unwrap();
    let a = doc.root();
    assert_eq!(doc.node_value(doc.children(a)[0]), "x\ny\nz");
}

#[test]
fn char_ref_cr_preserved_and_escaped_on_serialize() {
    let doc = XmlDocument::parse_strict("<a>x&#13;y</a>", &L).unwrap();
    let a = doc.root();
    assert_eq!(doc.node_value(doc.children(a)[0]), "x\ry");
    assert_eq!(doc.serialize(), "<a>x&#13;y</a>");
}

#[test]
fn attr_literal_ws_normalized_to_spaces_charrefs_escaped() {
    let doc = XmlDocument::parse_strict("<a b=\"x\ty\nz\rw\" c=\"&#9;&#10;&#13;\"/>", &L).unwrap();
    let a = doc.root();
    let bval = doc
        .attrs(a)
        .iter()
        .find(|(q, _)| q.local == "b")
        .map(|(_, v)| v.as_str());
    // 属性值中的字面 tab/newline/CR 全部规范化为空格。
    assert_eq!(bval, Some("x y z w"));
    // 字符引用保留为控制字符，序列化回转义。
    let ser = doc.serialize();
    assert!(ser.contains("c=\"&#9;&#10;&#13;\""), "序列化: {ser}");
}

#[test]
fn attr_quotes_serialized_double() {
    let doc = XmlDocument::parse_strict("<a b='say \"hi\"' c=\"it's\"/>", &L).unwrap();
    let ser = doc.serialize();
    assert!(
        ser.contains("b=\"say &quot;hi&quot;\"") && ser.contains("c=\"it's\""),
        "序列化: {ser}"
    );
}

// ---- 深度限额（strict 与 lenient 一致） ---------------------------------

fn nested(depth: usize) -> String {
    let open = "<a>".repeat(depth);
    let close = "</a>".repeat(depth);
    format!("{open}{close}")
}

#[test]
fn depth_512_ok_513_rejected_both_modes() {
    let ok512 = nested(512);
    XmlDocument::parse_strict(&ok512, &L).expect("512 层严格解析应通过");
    XmlDocument::parse_lenient(&ok512, &L).expect("512 层宽松解析应通过");

    let bad513 = nested(513);
    let es = strict_err(&bad513);
    assert!(
        matches!(es, XmlError::NestingLimitExceeded(512)),
        "strict 实际: {es:?}"
    );
    let el = XmlDocument::parse_lenient(&bad513, &L).expect_err("513 层宽松解析也应拒绝");
    assert!(
        matches!(el, XmlError::NestingLimitExceeded(512)),
        "lenient 实际: {el:?}"
    );
}

// ---- DTD / 实体安全 ----------------------------------------------------

#[test]
fn dtd_rejected_anywhere_both_modes() {
    let cases = [
        "<!DOCTYPE a [<!ENTITY e \"x\">]><a>&e;</a>",
        "<!doctype a><a/>",
        "<a><!DOCTYPE b></a>",
    ];
    for xml in cases {
        let es = strict_err(xml);
        assert!(
            matches!(es, XmlError::EntityForbidden(..)),
            "strict: {es:?}"
        );
        let el = XmlDocument::parse_lenient(xml, &L).expect_err("宽松模式也必须拒绝 DTD");
        assert!(
            matches!(el, XmlError::EntityForbidden(..)),
            "lenient: {el:?}"
        );
    }
}
