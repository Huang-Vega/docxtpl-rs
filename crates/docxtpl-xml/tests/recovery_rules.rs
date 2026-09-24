//! 宽松解析恢复规则 1–6 的实证用例（对齐 libxml2 recover 探针）。

use docxtpl_xml::{NodeKind, RecoveryKind, XmlDocument, XmlLimits};

const W: &str = "xmlns:w=\"urn:w\"";

/// 把片段包进 `<w:root>` 后宽松解析并序列化。
fn heal(content: &str) -> (String, Vec<RecoveryKind>) {
    let xml = format!("<w:root {W}>{content}</w:root>");
    let outcome = XmlDocument::parse_lenient(&xml, &XmlLimits::default())
        .unwrap_or_else(|e| panic!("宽松解析失败 {xml:?}: {e}"));
    let kinds = outcome.diagnostics.iter().map(|d| d.kind).collect();
    (outcome.doc.serialize(), kinds)
}

/// 宽松解析整段输入（用于根后杂散内容等不便包裹的用例）。
fn heal_raw(xml: &str) -> (String, Vec<RecoveryKind>) {
    let outcome = XmlDocument::parse_lenient(xml, &XmlLimits::default())
        .unwrap_or_else(|e| panic!("宽松解析失败 {xml:?}: {e}"));
    let kinds = outcome.diagnostics.iter().map(|d| d.kind).collect();
    (outcome.doc.serialize(), kinds)
}

fn kinds_contains(kinds: &[RecoveryKind], want: RecoveryKind) {
    assert!(kinds.contains(&want), "诊断 {kinds:?} 中应包含 {want:?}");
}

// ---- 规则 1：坏实体 ----------------------------------------------------

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
    // 内建实体缺分号也不识别。
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
    // 解码后再次序列化时 & < > 仍会被转义。
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

// ---- 规则 2：孤立 < -----------------------------------------------------

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

// ---- 规则 3：结束标签 ---------------------------------------------------

#[test]
fn rule3_simple_mismatch_autoclose_empty_descendant() {
    let (out, _) = heal("<a><b></a></b>");
    assert_eq!(out, format!("<w:root {W}><a><b/></a></w:root>"));
}

#[test]
fn rule3_autoclose_keeps_descendant_content() {
    // 依 prompt 实证（libxml2 6.1.1 钉版）：b 带内容自动闭合，w:t 随之闭合，d 落到上层。
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

// ---- 规则 4：开标签 -----------------------------------------------------

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
    // 无 = 的孤立属性名被忽略，元素正常开启。
    let (out, _) = heal("&a<b c>d");
    assert_eq!(out, format!("<w:root {W}><b>d</b></w:root>"));
}

#[test]
fn rule4_eof_inside_open_chain() {
    let (out, _) = heal("<a><b>text");
    assert_eq!(out, format!("<w:root {W}><a><b>text</b></a></w:root>"));
}

// ---- 规则 5：注释 / CDATA / PI -----------------------------------------

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

// ---- 规则 6：根后杂散内容 -----------------------------------------------

#[test]
fn rule6_tail_after_root_dropped() {
    let (out, kinds) = heal_raw("<a/>TAIL<b/>");
    assert_eq!(out, "<a/>");
    kinds_contains(&kinds, RecoveryKind::PrologTailDropped);
}

#[test]
fn rule6_whitespace_after_root_ok() {
    // 根后的合法空白不产生诊断；epilog 文本不属于树（lxml tostring 同样不输出）。
    let (out, kinds) = heal_raw("<a/>   \n  ");
    assert_eq!(out, "<a/>");
    assert!(!kinds.contains(&RecoveryKind::PrologTailDropped));
}

// ---- 组合：真实语料形态 ------------------------------------------------

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
    // lxml tostring：单引号、UTF-8、standalone=yes，声明后带换行。
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
