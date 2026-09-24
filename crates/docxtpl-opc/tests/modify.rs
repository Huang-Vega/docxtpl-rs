//! set_part_bytes：只有目标 part 的字节变化，其余不变；派生视图同步更新。

mod common;

use std::io::Cursor;

use common::*;
use docxtpl_opc::{OpcError, Package, PackageLimits};

fn open(bytes: &[u8]) -> Package {
    Package::from_reader(Cursor::new(bytes), &PackageLimits::default()).expect("打开包")
}

#[test]
fn set_part_bytes_changes_only_target() {
    let mut pkg = open(&minimal_docx());
    let before: Vec<(String, Vec<u8>)> = pkg
        .parts()
        .map(|part| (part.name().to_string(), part.bytes().to_vec()))
        .collect();
    let new_document = b"<?xml version=\"1.0\"?><w:document><w:body/></w:document>".to_vec();

    pkg.set_part_bytes("word/document.xml", new_document.clone())
        .expect("修改主文档");

    for (name, data) in &before {
        let part = pkg.part(name).expect("part 仍在");
        if name == "word/document.xml" {
            assert_eq!(part.bytes(), new_document);
            assert!(part.is_modified());
        } else {
            assert_eq!(part.bytes(), data, "其余 part 字节不变: {name}");
            assert!(!part.is_modified());
        }
    }

    // 写回后重开：只有该 part 的字节变化，顺序不变
    let mut out = Vec::new();
    pkg.write_to(Cursor::new(&mut out)).expect("写回");
    let reopened = open(&out);
    for (name, data) in &before {
        let part = reopened.part(name).expect("part 仍在");
        if name == "word/document.xml" {
            assert_eq!(part.bytes(), new_document);
        } else {
            assert_eq!(part.bytes(), data, "写出不应改动 {name}");
        }
    }
}

#[test]
fn set_part_bytes_missing_part() {
    let mut pkg = open(&minimal_docx());
    let err = pkg
        .set_part_bytes("word/missing.xml", vec![1, 2, 3])
        .expect_err("不存在的 part 应报错");
    assert!(
        matches!(err, OpcError::PartNotFound { ref uri } if uri == "word/missing.xml"),
        "实际: {err:?}"
    );
}

#[test]
fn set_part_bytes_reloads_rels_and_content_types() {
    let mut pkg = open(&minimal_docx());

    // 修改 word/_rels/document.xml.rels → relationships_of 反映新内容
    let new_rels = concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
        r#"<Relationship Id="rId9" Type="http://example.com/rel" Target="styles.xml"/>"#,
        "</Relationships>"
    );
    pkg.set_part_bytes("word/_rels/document.xml.rels", new_rels.as_bytes().to_vec())
        .expect("更新 rels");
    let rels = pkg
        .relationships_of("word/document.xml")
        .expect("关系视图应更新");
    assert_eq!(rels.len(), 1);
    assert!(rels.get("rId9").is_some());
    assert!(rels.get("rId1").is_none());
    pkg.validate()
        .expect("新 rels 合法（指向 word/styles.xml）");

    // 修改 [Content_Types].xml → content_types() 反映新内容
    let new_ct = concat!(
        "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">",
        "<Default Extension=\"xml\" ContentType=\"text/plain\"/>",
        "</Types>"
    );
    pkg.set_part_bytes("[Content_Types].xml", new_ct.as_bytes().to_vec())
        .expect("更新 Content Types");
    let styles_uri = pkg.part("word/styles.xml").unwrap().uri().clone();
    assert_eq!(
        pkg.content_types().content_type_of(&styles_uri),
        Some("text/plain")
    );

    // 修改 _rels/.rels → root_relationships() 反映新内容
    let new_root = concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
        r#"<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>"#,
        "</Relationships>"
    );
    pkg.set_part_bytes("_rels/.rels", new_root.as_bytes().to_vec())
        .expect("更新根 rels");
    assert_eq!(pkg.root_relationships().len(), 1);
    assert_eq!(
        pkg.main_document_uri().expect("主文档 URI").as_str(),
        "word/document.xml"
    );
    pkg.validate().expect("包仍然合法");
}

#[test]
fn set_part_bytes_invalid_rels_is_rejected_and_keeps_old_bytes() {
    let mut pkg = open(&minimal_docx());
    // 缺少 Type/Target 的非法关系
    let bad = concat!(
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
        r#"<Relationship Id="rId1"/>"#,
        "</Relationships>"
    );
    let err = pkg
        .set_part_bytes("word/_rels/document.xml.rels", bad.as_bytes().to_vec())
        .expect_err("非法 rels 应失败");
    assert!(
        matches!(err, OpcError::InvalidRelationships { .. }),
        "实际: {err:?}"
    );
    // 字节未变
    assert_eq!(
        pkg.part("word/_rels/document.xml.rels").unwrap().bytes(),
        DOCUMENT_RELS_XML.as_bytes()
    );
}
