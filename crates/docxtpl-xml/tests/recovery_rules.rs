//! Empirical tests for lenient-parse recovery rules 1–6 (aligned with the
//! libxml2 recover probes).

use docxtpl_xml::{NodeKind, RecoveryKind, XmlDocument, XmlLimits};

const W: &str = "xmlns:w=\"urn:w\"";

/// Wrap a fragment in `<w:root>`, parse leniently and serialize.
fn heal(content: &str) -> (String, Vec<RecoveryKind>) {
    let xml = format!("<w:root {W}>{content}</w:root>");
    let outcome = XmlDocument::parse_lenient(&xml, &XmlLimits::default())
        .unwrap_or_else(|e| panic!("lenient parsing failed for {xml:?}: {e}"));
    let kinds = outcome.diagnostics.iter().map(|d| d.kind).collect();
    (outcome.doc.serialize(), kinds)
}

/// Leniently parse a whole input as-is (for cases such as post-root stray
/// content that cannot easily be wrapped).
fn heal_raw(xml: &str) -> (String, Vec<RecoveryKind>) {
    let outcome = XmlDocument::parse_lenient(xml, &XmlLimits::default())
        .unwrap_or_else(|e| panic!("lenient parsing failed for {xml:?}: {e}"));
    let kinds = outcome.diagnostics.iter().map(|d| d.kind).collect();
    (outcome.doc.serialize(), kinds)
}

fn kinds_contains(kinds: &[RecoveryKind], want: RecoveryKind) {
    assert!(
        kinds.contains(&want),
        "diagnostics {kinds:?} should contain {want:?}"
    );
}

// ---- rule 1: bad entities -----------------------------------------------

#[test]
fn rule1_amp_plain() {
    let (out, kinds) = heal("a&b c");
    assert_eq!(out, format!("<w:root {W}>a c</w:root>"));
    kinds_contains(&kinds, RecoveryKind::BadEntityDropped);
}

#[test]
fn rule1_amp_two() {
    let (out, kinds) = heal("x &y z&q;w");
    assert_eq!(out, format!("<w:root {W}>x  zw</w:root>"));
    assert_eq!(
        kinds
            .iter()
            .filter(|k| **k == RecoveryKind::BadEntityDropped)
            .count(),
        2
    );
}

#[test]
fn rule1_amp_without_semicolon() {
    // A built-in entity without a semicolon is not recognized either.
    let (out, _) = heal("x&amp y");
    assert_eq!(out, format!("<w:root {W}>x y</w:root>"));
}

#[test]
fn rule1_double_amp() {
    let (out, kinds) = heal("x && y");
    assert_eq!(out, format!("<w:root {W}>x  y</w:root>"));
    assert_eq!(
        kinds
            .iter()
            .filter(|k| **k == RecoveryKind::BadEntityDropped)
            .count(),
        2
    );
}

#[test]
fn rule1_amp_only() {
    let (out, _) = heal("&");
    assert_eq!(out, format!("<w:root {W}/>"));
}

#[test]
fn rule1_amp_semicolon_kept_when_no_name() {
    let (out, _) = heal("&;");
    assert_eq!(out, format!("<w:root {W}>;</w:root>"));
}

#[test]
fn rule1_valid_entities_decode() {
    let (out, kinds) = heal("a&amp;b&#65;c&#x41;d&apos;e&quot;f&lt;g&gt;");
    // When the decoded output is serialized again, & < > are still escaped.
    assert_eq!(
        out,
        format!("<w:root {W}>a&amp;bAcAd'e\"f&lt;g&gt;</w:root>")
    );
    assert!(!kinds.contains(&RecoveryKind::BadEntityDropped));
}

#[test]
fn rule1_dot_is_not_name_start() {
    let (out, _) = heal("a&.b;c");
    assert_eq!(out, format!("<w:root {W}>a.b;c</w:root>"));
}

// ---- rule 2: stray < ----------------------------------------------------

#[test]
fn rule2_digit_space_eof() {
    let (out, kinds) = heal("3<5 x<y");
    assert_eq!(out, format!("<w:root {W}>35 x<y/></w:root>"));
    kinds_contains(&kinds, RecoveryKind::MalformedTag);
    kinds_contains(&kinds, RecoveryKind::TagAutoClosed);
}

#[test]
fn rule2_spaces_keep_gt_as_text() {
    let (out, _) = heal("a < b > c");
    assert_eq!(out, format!("<w:root {W}>a  b &gt; c</w:root>"));
}

#[test]
fn rule2_double_lt() {
    let (out, _) = heal("<<x>>");
    assert_eq!(out, format!("<w:root {W}><x>&gt;</x></w:root>"));
}

// ---- rule 3: end tags ---------------------------------------------------

#[test]
fn rule3_simple_mismatch_autoclose_empty_descendant() {
    let (out, _) = heal("<a><b></a></b>");
    assert_eq!(out, format!("<w:root {W}><a><b/></a></w:root>"));
}

#[test]
fn rule3_autoclose_keeps_descendant_content() {
    // Per empirical probing (libxml2 pinned at 6.1.1): b with content is
    // auto-closed, w:t closes along with it, and d lands one level up.
    let (out, kinds) = heal("<w:t>a<b>c</w:t>d");
    assert_eq!(out, format!("<w:root {W}><w:t>a<b>c</b></w:t>d</w:root>"));
    kinds_contains(&kinds, RecoveryKind::TagAutoClosed);
}

#[test]
fn rule3_stray_close_pops_stack_top() {
    let (out, kinds) = heal("<w:r><w:t>a</b>c</w:t>TAIL</w:r>");
    assert_eq!(
        out,
        format!("<w:root {W}><w:r><w:t>a</w:t>c</w:r>TAIL</w:root>")
    );
    kinds_contains(&kinds, RecoveryKind::StrayEndTag);
}

#[test]
fn rule3_stray_close_at_root_drops_rest() {
    let (out, kinds) = heal("a</b>c");
    assert_eq!(out, format!("<w:root {W}>a</w:root>"));
    kinds_contains(&kinds, RecoveryKind::StrayEndTag);
    kinds_contains(&kinds, RecoveryKind::PrologTailDropped);
}

#[test]
fn rule3_malformed_end_tag_acts_as_stray() {
    let (out, kinds) = heal("a</ b>c");
    assert_eq!(out, format!("<w:root {W}>a</w:root>"));
    kinds_contains(&kinds, RecoveryKind::StrayEndTag);
}

#[test]
fn rule3_deep_chain_autoclose() {
    let (out, _) = heal("<a><b><c></a>");
    assert_eq!(out, format!("<w:root {W}><a><b><c/></b></a></w:root>"));
}

// ---- rule 4: opening tags -----------------------------------------------

#[test]
fn rule4_unclosed_at_eof() {
    let (out, kinds) = heal("text<unclosed");
    assert_eq!(out, format!("<w:root {W}>text<unclosed/></w:root>"));
    kinds_contains(&kinds, RecoveryKind::TagAutoClosed);
}

#[test]
fn rule4_lt_before_gt_autoclose() {
    let (out, kinds) = heal("a<y</w:t>");
    assert_eq!(out, format!("<w:root {W}>a<y/></w:root>"));
    kinds_contains(&kinds, RecoveryKind::TagAutoClosed);
}

#[test]
fn rule4_unquoted_attr_produces_void_and_resumes_at_value() {
    let (out, kinds) = heal("<a href=x>t</a>");
    assert_eq!(out, format!("<w:root {W}><a/>x&gt;t</w:root>"));
    kinds_contains(&kinds, RecoveryKind::MalformedTag);
}

#[test]
fn rule4_malformed_attr_amp_resumes_in_place() {
    let (out, kinds) = heal("a<b && c>d");
    assert_eq!(out, format!("<w:root {W}>a<b/> c&gt;d</w:root>"));
    kinds_contains(&kinds, RecoveryKind::MalformedTag);
}

#[test]
fn rule4_legal_boolean_attr_ignored() {
    // A standalone attribute name without '=' is ignored; the element opens
    // normally.
    let (out, _) = heal("&a<b c>d");
    assert_eq!(out, format!("<w:root {W}><b>d</b></w:root>"));
}

#[test]
fn rule4_eof_inside_open_chain() {
    let (out, _) = heal("<a><b>text");
    assert_eq!(out, format!("<w:root {W}><a><b>text</b></a></w:root>"));
}

// ---- rule 5: comments / CDATA / PI --------------------------------------

#[test]
fn rule5_comment_preserved() {
    let (out, _) = heal("a<!-- c -- -->b");
    assert_eq!(out, format!("<w:root {W}>a<!-- c -- -->b</w:root>"));
}

#[test]
fn rule5_comment_node_value() {
    let xml = format!("<w:root {W}>a<!-- hi -->b</w:root>");
    let doc = XmlDocument::parse_lenient(&xml, &XmlLimits::default())
        .unwrap()
        .doc;
    let root = doc.root();
    let kids = doc.children(root);
    assert_eq!(doc.node_kind(kids[1]), NodeKind::Comment);
    assert_eq!(doc.node_value(kids[1]), " hi ");
}

#[test]
fn rule5_unterminated_comment_dropped() {
    let (out, kinds) = heal("a<!-- nope");
    assert_eq!(out, format!("<w:root {W}>a</w:root>"));
    kinds_contains(&kinds, RecoveryKind::MalformedTag);
}

#[test]
fn rule5_cdata_becomes_escaped_text() {
    let (out, _) = heal("a<![CDATA[x<y & z]]>b");
    assert_eq!(out, format!("<w:root {W}>ax&lt;y &amp; zb</w:root>"));
}

#[test]
fn rule5_unterminated_cdata_dropped() {
    let (out, _) = heal("a<![CDATA[xy");
    assert_eq!(out, format!("<w:root {W}>a</w:root>"));
}

#[test]
fn rule5_pi_preserved_unterminated_dropped() {
    let (ok, _) = heal("a<?pi target?>b");
    assert_eq!(ok, format!("<w:root {W}>a<?pi target?>b</w:root>"));
    let (eof, kinds) = heal("a<?pi x");
    assert_eq!(eof, format!("<w:root {W}>a</w:root>"));
    kinds_contains(&kinds, RecoveryKind::MalformedTag);
}

// ---- rule 6: post-root stray content ------------------------------------

#[test]
fn rule6_tail_after_root_dropped() {
    let (out, kinds) = heal_raw("<a/>TAIL<b/>");
    assert_eq!(out, "<a/>");
    kinds_contains(&kinds, RecoveryKind::PrologTailDropped);
}

#[test]
fn rule6_whitespace_after_root_ok() {
    // Legal whitespace after the root produces no diagnostic; epilog text
    // is not part of the tree (lxml tostring does not output it either).
    let (out, kinds) = heal_raw("<a/>   \n  ");
    assert_eq!(out, "<a/>");
    assert!(!kinds.contains(&RecoveryKind::PrologTailDropped));
}

// ---- combined: real-corpus shapes ---------------------------------------

#[test]
fn real_corpus_att() {
    let (out, _) = heal("Company: AT&T Inc");
    assert_eq!(out, format!("<w:root {W}>Company: AT Inc</w:root>"));
}

#[test]
fn real_corpus_gt_always_escaped() {
    let (out, _) = heal("R > 50%");
    assert_eq!(out, format!("<w:root {W}>R &gt; 50%</w:root>"));
}

#[test]
fn attr_bad_entity_dropped_in_lenient() {
    let (out, _) = heal(r#"<a b="&bad;"/>x"#);
    assert_eq!(out, format!("<w:root {W}><a b=\"\"/>x</w:root>"));
}

#[test]
fn declaration_serialized_lxml_style_when_present() {
    // lxml tostring: single quotes, UTF-8, standalone=yes, followed by a
    // newline.
    let (out, _) = heal_raw("<?xml version='1.0' standalone='yes'?><a/>");
    assert_eq!(
        out,
        "<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n<a/>"
    );
}

#[test]
fn no_declaration_no_prolog_output() {
    let (out, _) = heal_raw("<a/>");
    assert_eq!(out, "<a/>");
}
