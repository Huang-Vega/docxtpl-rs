//! 序列化器的 lxml 冗余命名空间声明裁剪测试（ADR-005 §2，探针 A/B/C/D）。

use docxtpl_xml::{XmlDocument, XmlLimits};

fn roundtrip(xml: &str) -> String {
    XmlDocument::parse_strict(xml, &XmlLimits::default())
        .expect("应解析成功")
        .serialize()
}

#[test]
fn probe_a_redundant_prefixes_on_ancestors_are_pruned() {
    // 宿主声明 wp/r，子树自带四声明 → 只剩 a、pic（保原相对顺序）。
    let out = roundtrip(
        r#"<w:document xmlns:w="W" xmlns:wp="WP" xmlns:r="R">
             <w:body><w:p><wp:inline xmlns:wp="WP" xmlns:a="A" xmlns:pic="PIC" xmlns:r="R"><a:foo><pic:bar/></a:foo></wp:inline></w:p></w:body>
           </w:document>"#,
    );
    let inline = out.find("<wp:inline").expect("应存在 wp:inline");
    assert!(
        !out[inline..].starts_with("<wp:inline xmlns:wp=\"WP\""),
        "祖先已有 wp 绑定，应被裁剪：{out}"
    );
    let seg = &out[inline..inline + 60];
    assert!(seg.contains("xmlns:a=\"A\"") && seg.contains("xmlns:pic=\"PIC\""));
    assert!(seg.find("xmlns:a").unwrap() < seg.find("xmlns:pic").unwrap());
}

#[test]
fn probe_b_different_uri_binding_is_kept() {
    // 祖先 a=OTHER，子树 a=A → 保留子树声明。
    let out = roundtrip(
        r#"<w:document xmlns:w="W" xmlns:a="OTHER"><w:body><wp:inline xmlns:wp="WP" xmlns:a="A"><a:foo/></wp:inline></w:body></w:document>"#,
    );
    assert!(out.contains("<wp:inline xmlns:wp=\"WP\" xmlns:a=\"A\">"));
}

#[test]
fn probe_c_sibling_subtrees_keep_own_declarations() {
    // 裁剪只看祖先轴；兄弟子树各自保留声明。
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
        "兄弟不参与裁剪：{out}"
    );
}

#[test]
fn probe_d_default_namespace_binding_pruned_when_equal() {
    // 默认前缀（空前缀）同样处理：祖先 xmlns="X"，子树 xmlns="X" 被裁。
    let out = roundtrip(r#"<root xmlns="X"><child/><node xmlns="X"><inner/></node></root>"#);
    assert!(
        out.contains("<node><inner/></node>"),
        "空前缀冗余应被裁：{out}"
    );
}

#[test]
fn probe_e_pruning_is_not_transitive_into_children() {
    // 子树声明被裁后，其内部的声明仍以"祖先轴"为准：
    // <p2> 在被裁的子树内自带 a 声明（与被裁声明同 URI）→ 也被裁。
    let out = roundtrip(
        r#"<w:document xmlns:w="W" xmlns:a="A">
             <w:body><n xmlns:a="A"><m xmlns:a="A"/></n></w:body>
           </w:document>"#,
    );
    assert!(out.contains("<n><m/></n>"), "{out}");
}
