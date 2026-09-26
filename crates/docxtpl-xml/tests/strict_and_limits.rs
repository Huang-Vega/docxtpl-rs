//! Tests for strict-parsing well-formedness, CR normalization, depth limits
//! and DTD security policy.

use docxtpl_xml::{XmlDocument, XmlError, XmlLimits};

const L: XmlLimits = XmlLimits { max_depth: 512 };

fn strict_err(xml: &str) -> XmlError {
    XmlDocument::parse_strict(xml, &L).expect_err("strict parsing should have failed")
}

// ---- strict-mode well-formedness ---------------------------------------

#[test]
fn strict_rejects_unknown_entity() {
    let e = strict_err("<a>&bogus;</a>");
    assert!(matches!(e, XmlError::Parse { .. }), "actual: {e:?}");
}

#[test]
fn strict_rejects_bare_amp() {
    assert!(matches!(strict_err("<a>a & b</a>"), XmlError::Parse { .. }));
}

#[test]
fn strict_rejects_duplicate_attr() {
    let e = strict_err(r#"<a x="1" x="2"/>"#);
    assert!(matches!(e, XmlError::Parse { .. }), "actual: {e:?}");
}

#[test]
fn strict_rejects_unbound_prefix() {
    let e = strict_err("<x:a/>");
    assert!(matches!(e, XmlError::Parse { .. }), "actual: {e:?}");
}

#[test]
fn strict_rejects_mismatched_tag() {
    let e = strict_err("<a><b></a></b>");
    assert!(matches!(e, XmlError::Parse { .. }), "actual: {e:?}");
}

#[test]
fn strict_rejects_garbage_after_root() {
    let e = strict_err("<a/>tail");
    assert!(matches!(e, XmlError::Parse { .. }), "actual: {e:?}");
}

#[test]
fn strict_rejects_unclosed() {
    // Unclosed-at-EOF may be reported as either Incomplete or a located
    // Parse; both are acceptable.
    assert!(matches!(
        strict_err("<a><b>text"),
        XmlError::Incomplete(..) | XmlError::Parse { .. }
    ));
}

#[test]
fn strict_accepts_decl_comments_pi_and_whitespace() {
    let xml = "<?xml version=\"1.0\"?>\n<!-- hi --><?app data?>\n<a>x</a>\n";
    let doc = XmlDocument::parse_strict(xml, &L).expect("should pass strict parsing");
    assert!(doc.serialize().starts_with("<?xml version='1.0'"));
}

// ---- CR and attribute whitespace normalization --------------------------

#[test]
fn literal_cr_normalized_in_text() {
    // Literal \r and \r\n are normalized to \n at the parsing layer.
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
    // Literal tab/newline/CR in an attribute value are all normalized to
    // spaces.
    assert_eq!(bval, Some("x y z w"));
    // Character references stay as control characters and are re-escaped on
    // serialization.
    let ser = doc.serialize();
    assert!(ser.contains("c=\"&#9;&#10;&#13;\""), "serialized: {ser}");
}

#[test]
fn attr_quotes_serialized_double() {
    let doc = XmlDocument::parse_strict("<a b='say \"hi\"' c=\"it's\"/>", &L).unwrap();
    let ser = doc.serialize();
    assert!(
        ser.contains("b=\"say &quot;hi&quot;\"") && ser.contains("c=\"it's\""),
        "serialized: {ser}"
    );
}

#[test]
fn bounded_serializer_stops_entity_expansion() {
    let doc = XmlDocument::parse_strict("<a>&gt;&gt;</a>", &L).unwrap();
    let expected = doc.serialize();
    assert_eq!(
        doc.try_serialize(expected.len()).unwrap(),
        expected,
        "bounded and unbounded serializers must be byte-identical"
    );
    let error = doc.try_serialize(expected.len() - 1).unwrap_err();
    assert_eq!(error.max, expected.len() - 1);
}

// ---- depth limits (same for strict and lenient) -------------------------

fn nested(depth: usize) -> String {
    let open = "<a>".repeat(depth);
    let close = "</a>".repeat(depth);
    format!("{open}{close}")
}

#[test]
fn depth_512_ok_513_rejected_both_modes() {
    let ok512 = nested(512);
    XmlDocument::parse_strict(&ok512, &L).expect("512 levels should pass strict parsing");
    XmlDocument::parse_lenient(&ok512, &L).expect("512 levels should pass lenient parsing");

    let bad513 = nested(513);
    let es = strict_err(&bad513);
    assert!(
        matches!(es, XmlError::NestingLimitExceeded(512)),
        "strict actual: {es:?}"
    );
    let el = XmlDocument::parse_lenient(&bad513, &L)
        .expect_err("513 levels must also be rejected in lenient parsing");
    assert!(
        matches!(el, XmlError::NestingLimitExceeded(512)),
        "lenient actual: {el:?}"
    );
}

// ---- DTD / entity security ----------------------------------------------

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
        let el =
            XmlDocument::parse_lenient(xml, &L).expect_err("lenient mode must also reject DTD");
        assert!(
            matches!(el, XmlError::EntityForbidden(..)),
            "lenient: {el:?}"
        );
    }
}
