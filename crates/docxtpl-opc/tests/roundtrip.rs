//! 合成 docx 往返：打开 → 断言 → 写回 → 重开，未修改 part 字节逐一相等。

mod common;

use std::io::Cursor;

use common::*;
use docxtpl_opc::{Package, PackageLimits, Relationships, TargetMode};

const EXPECTED_NAMES: [&str; 5] = [
    "[Content_Types].xml",
    "_rels/.rels",
    "word/document.xml",
    "word/_rels/document.xml.rels",
    "word/styles.xml",
];

fn open(bytes: &[u8]) -> Package {
    Package::from_reader(Cursor::new(bytes), &PackageLimits::default()).expect("打开包")
}

#[test]
fn minimal_docx_roundtrip() {
    let original = minimal_docx();
    let pkg = open(&original);

    // part 集与顺序
    let names: Vec<&str> = pkg.parts().map(|part| part.name()).collect();
    assert_eq!(names, EXPECTED_NAMES);
    assert_eq!(pkg.part_count(), 5);
    assert!(pkg.contains("word/document.xml"));
    assert!(pkg.contains("[Content_Types].xml"));
    assert!(!pkg.contains("word/missing.xml"));
    assert!(pkg.parts().all(|part| !part.is_modified()));
    assert!(pkg.parts().all(|part| !part.is_dir()));

    // part 的关系
    let doc = pkg.part("word/document.xml").expect("主文档存在");
    assert_eq!(doc.uri().as_str(), "word/document.xml");
    assert_eq!(doc.name(), "word/document.xml");
    let rels = doc.relationships().expect("主文档应有关系");
    assert_eq!(rels.len(), 2);
    assert!(!rels.is_empty());
    let styles = rels.get("rId1").expect("rId1");
    assert_eq!(styles.id, "rId1");
    assert_eq!(
        styles.rel_type,
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles"
    );
    assert_eq!(styles.target, "styles.xml");
    assert_eq!(styles.target_mode, TargetMode::Internal);
    let link = rels.get("rId2").expect("rId2");
    assert_eq!(link.target, "https://example.com/doc");
    assert_eq!(link.target_mode, TargetMode::External);

    // resolve：target 相对 part 所在目录规范化
    let doc_uri = doc.uri().clone();
    assert_eq!(
        rels.resolve(&doc_uri, "rId1").expect("resolve").as_str(),
        "word/styles.xml"
    );
    // 外部关系不参与包内解析
    assert!(rels.resolve(&doc_uri, "rId2").is_none());
    assert!(rels.resolve(&doc_uri, "missing").is_none());
    // 顺序保持原文件
    let ids: Vec<&str> = rels.iter().map(|rel| rel.id.as_str()).collect();
    assert_eq!(ids, ["rId1", "rId2"]);
    assert_eq!(
        pkg.relationships_of("word/document.xml")
            .map(Relationships::len),
        Some(2)
    );
    assert!(pkg.relationships_of("word/styles.xml").is_none());

    // 根关系与主文档
    assert_eq!(pkg.root_relationships().len(), 1);
    assert_eq!(
        pkg.main_document_uri().expect("主文档 URI").as_str(),
        "word/document.xml"
    );

    // content types
    assert_eq!(
        pkg.content_types().content_type_of(&doc_uri),
        Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml")
    );
    assert_eq!(
        pkg.content_types()
            .content_type_of(pkg.part("word/styles.xml").unwrap().uri()),
        Some("application/xml")
    );
    assert_eq!(
        pkg.content_types()
            .content_type_of(pkg.part("word/_rels/document.xml.rels").unwrap().uri()),
        Some("application/vnd.openxmlformats-package.relationships+xml")
    );

    // 完整性
    pkg.validate().expect("最小 docx 应通过校验");

    // 写回（内存流）
    let mut out = Vec::new();
    pkg.write_to(Cursor::new(&mut out)).expect("write_to");
    let reopened = open(&out);
    let names_after: Vec<&str> = reopened.parts().map(|part| part.name()).collect();
    assert_eq!(names_after, EXPECTED_NAMES, "写出后条目顺序一致");
    for part in pkg.parts() {
        assert_eq!(
            part.bytes(),
            reopened
                .part(part.name())
                .expect("重开应有同名 part")
                .bytes(),
            "part {} 未修改，字节应逐一相等",
            part.name()
        );
    }

    // 落盘保存后重开
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("out.docx");
    pkg.save(&path).expect("save");
    let from_disk = Package::open(&path, &PackageLimits::default()).expect("重新打开");
    assert_eq!(from_disk.part_count(), 5);
    for part in pkg.parts() {
        assert_eq!(
            part.bytes(),
            from_disk.part(part.name()).expect("part 仍在").bytes()
        );
    }
}

#[test]
fn directory_entries_roundtrip() {
    let bytes = build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES_XML.as_bytes()),
        ("_rels/.rels", ROOT_RELS_XML.as_bytes()),
        ("word/", b""),
        ("word/document.xml", DOCUMENT_XML.as_bytes()),
        ("word/_rels/document.xml.rels", EMPTY_RELS_XML.as_bytes()),
    ]);
    let pkg = open(&bytes);

    let dir_part = pkg.part("word/").expect("目录条目应保留");
    assert!(dir_part.is_dir());
    assert!(dir_part.bytes().is_empty());
    assert!(dir_part.uri().parent().is_none());
    assert_eq!(dir_part.uri().file_name(), "word");
    assert!(dir_part.relationships().is_none());

    // 空 rels 也构成关系视图（空集合）
    let doc = pkg.part("word/document.xml").expect("主文档存在");
    assert_eq!(doc.relationships().map(Relationships::len), Some(0));

    pkg.validate().expect("含目录条目应通过校验");

    let mut out = Vec::new();
    pkg.write_to(Cursor::new(&mut out)).expect("写回");
    let reopened = open(&out);
    let dir_after = reopened.part("word/").expect("目录条目应写回");
    assert!(dir_after.is_dir());
    assert!(dir_after.bytes().is_empty());
    assert_eq!(
        reopened
            .part("word/document.xml")
            .expect("主文档仍在")
            .bytes(),
        DOCUMENT_XML.as_bytes()
    );
}
