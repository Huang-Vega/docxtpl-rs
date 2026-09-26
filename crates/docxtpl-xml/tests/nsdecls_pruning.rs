//! Tests for the serializer's lxml-style pruning of redundant namespace
//! declarations (ADR-005 §2, probes A/B/C/D).

use docxtpl_xml::{XmlDocument, XmlLimits};

fn roundtrip(xml: &str) -> String {
    XmlDocument::parse_strict(xml, &XmlLimits::default())
        .expect("parsing should succeed")
        .serialize()
}

#[test]
fn probe_a_redundant_prefixes_on_ancestors_are_pruned() {
    // The host declares wp/r and the subtree carries all four declarations
    // → only a and pic remain (in their original relative order).
    let out = roundtrip(
        r#"<w:document xmlns:w="W" xmlns:wp="WP" xmlns:r="R">
             <w:body><w:p><wp:inline xmlns:wp="WP" xmlns:a="A" xmlns:pic="PIC" xmlns:r="R"><a:foo><pic:bar/></a:foo></wp:inline></w:p></w:body>
           </w:document>"#,
    );
    let inline = out.find("<wp:inline").expect("wp:inline should exist");
    assert!(
        !out[inline..].starts_with("<wp:inline xmlns:wp=\"WP\""),
        "ancestor already binds wp, so it should be pruned: {out}"
    );
    let seg = &out[inline..inline + 60];
    assert!(seg.contains("xmlns:a=\"A\"") && seg.contains("xmlns:pic=\"PIC\""));
    assert!(seg.find("xmlns:a").unwrap() < seg.find("xmlns:pic").unwrap());
}

#[test]
fn probe_b_different_uri_binding_is_kept() {
    // Ancestor a=OTHER, subtree a=A → keep the subtree declaration.
    let out = roundtrip(
        r#"<w:document xmlns:w="W" xmlns:a="OTHER"><w:body><wp:inline xmlns:wp="WP" xmlns:a="A"><a:foo/></wp:inline></w:body></w:document>"#,
    );
    assert!(out.contains("<wp:inline xmlns:wp=\"WP\" xmlns:a=\"A\">"));
}

#[test]
fn probe_c_sibling_subtrees_keep_own_declarations() {
    // Pruning only looks at the ancestor axis; sibling subtrees each keep
    // their own declarations.
    let out = roundtrip(
        r#"<w:document xmlns:w="W"><w:body>
             <wp:inline xmlns:wp="WP" xmlns:a="A"><a:x/></wp:inline>
             <wp:inline xmlns:wp="WP" xmlns:a="A"><a:x/></wp:inline>
           </w:body></w:document>"#,
    );
    assert_eq!(
        out.matches("wp:inline xmlns:wp=\"WP\" xmlns:a=\"A\"")
            .count(),
        2,
        "siblings do not participate in pruning: {out}"
    );
}

#[test]
fn probe_d_default_namespace_binding_pruned_when_equal() {
    // The default prefix (empty prefix) is handled the same way: ancestor
    // xmlns="X", subtree xmlns="X" gets pruned.
    let out = roundtrip(r#"<root xmlns="X"><child/><node xmlns="X"><inner/></node></root>"#);
    assert!(
        out.contains("<node><inner/></node>"),
        "redundant empty-prefix declaration should be pruned: {out}"
    );
}

#[test]
fn probe_e_pruning_is_not_transitive_into_children() {
    // After a subtree declaration is pruned, declarations inside it are
    // still evaluated against the "ancestor axis":
    // <p2> carries an a declaration inside the pruned subtree (same URI as
    // the pruned declaration) → it is pruned too.
    let out = roundtrip(
        r#"<w:document xmlns:w="W" xmlns:a="A">
             <w:body><n xmlns:a="A"><m xmlns:a="A"/></n></w:body>
           </w:document>"#,
    );
    assert!(out.contains("<n><m/></n>"), "{out}");
}
