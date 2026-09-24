//! validate()/main_document_uri()：悬空关系、外部目标、重复 Id、content type 覆盖。

mod common;

use std::io::Cursor;

use common::*;
use docxtpl_opc::{OpcError, Package, PackageLimits};

fn open(bytes: &[u8]) -> Package {
    Package::from_reader(Cursor::new(bytes), &PackageLimits::default()).expect("打开包")
}

fn rels_with(id: &str, target: &str) -> String {
    format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
            r#"<Relationship Id="{id}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="{target}"/>"#,
            "</Relationships>"
        ),
        id = id,
        target = target
    )
}

#[test]
fn dangling_internal_relationship_fails_validate() {
    // word/_rels/document.xml.rels 指向 styles.xml，但包内没有 word/styles.xml
    let bytes = build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES_XML.as_bytes()),
        ("_rels/.rels", ROOT_RELS_XML.as_bytes()),
        ("word/document.xml", DOCUMENT_XML.as_bytes()),
        (
            "word/_rels/document.xml.rels",
            rels_with("rId1", "styles.xml").as_bytes(),
        ),
    ]);
    let pkg = open(&bytes);
    // resolve 只做路径解析，不查存在性
    let doc = pkg.part("word/document.xml").unwrap();
    assert_eq!(
        doc.relationships()
            .unwrap()
            .resolve(doc.uri(), "rId1")
            .unwrap()
            .as_str(),
        "word/styles.xml"
    );
    let err = pkg.validate().expect_err("悬空关系应失败");
    assert!(
        matches!(err, OpcError::InvalidRelationships { .. } if err.to_string().contains("styles.xml")),
        "实际: {err:?}"
    );
}

#[test]
fn escaping_relationship_target_fails_validate() {
    let bytes = build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES_XML.as_bytes()),
        ("_rels/.rels", ROOT_RELS_XML.as_bytes()),
        ("word/document.xml", DOCUMENT_XML.as_bytes()),
        (
            "word/_rels/document.xml.rels",
            rels_with("rId1", "../../escape.xml").as_bytes(),
        ),
    ]);
    let err = open(&bytes).validate().expect_err("逃出包根的目标应失败");
    assert!(
        matches!(err, OpcError::InvalidRelationships { .. }),
        "实际: {err:?}"
    );
}

#[test]
fn external_relationship_passes_validate() {
    // minimal_docx 的 document.xml.rels 含 rId2（External）
    let pkg = open(&minimal_docx());
    let link = pkg
        .relationships_of("word/document.xml")
        .unwrap()
        .get("rId2")
        .unwrap()
        .clone();
    assert_eq!(link.target, "https://example.com/doc");
    assert_eq!(link.target_mode, docxtpl_opc::TargetMode::External);
    pkg.validate().expect("外部关系不参与存在性校验");
}

#[test]
fn duplicate_relationship_id_fails_validate() {
    let dup_rels = concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
        r#"<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>"#,
        r#"<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>"#,
        "</Relationships>"
    );
    let bytes = build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES_XML.as_bytes()),
        ("_rels/.rels", ROOT_RELS_XML.as_bytes()),
        ("word/document.xml", DOCUMENT_XML.as_bytes()),
        ("word/_rels/document.xml.rels", dup_rels.as_bytes()),
        ("word/styles.xml", STYLES_XML.as_bytes()),
    ]);
    let pkg = open(&bytes);
    // 两条关系都能解析到存在的 part，但 Id 重复
    let err = pkg.validate().expect_err("重复 Id 应失败");
    assert!(
        matches!(err, OpcError::InvalidRelationships { .. } if err.to_string().contains("rId1")),
        "实际: {err:?}"
    );
}

#[test]
fn missing_content_type_fails_validate() {
    let bytes = build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES_XML.as_bytes()),
        ("_rels/.rels", ROOT_RELS_XML.as_bytes()),
        ("word/document.xml", DOCUMENT_XML.as_bytes()),
        ("word/_rels/document.xml.rels", EMPTY_RELS_XML.as_bytes()),
        ("media/image.png", b"\x89PNG fake"),
    ]);
    let err = open(&bytes)
        .validate()
        .expect_err("png 无 content type 应失败");
    assert!(
        matches!(err, OpcError::InvalidContentTypes { .. } if err.to_string().contains("media/image.png")),
        "实际: {err:?}"
    );
}

#[test]
fn rels_and_content_types_exempt_from_content_type_check() {
    // .rels 与 [Content_Types].xml 自身不要求有 content type（本包也没有为 png 声明，
    // 但只含被豁免的 part 时不报错）
    let bytes = build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES_XML.as_bytes()),
        ("_rels/.rels", ROOT_RELS_XML.as_bytes()),
        ("word/document.xml", DOCUMENT_XML.as_bytes()),
        ("word/_rels/document.xml.rels", EMPTY_RELS_XML.as_bytes()),
    ]);
    open(&bytes)
        .validate()
        .expect("被豁免的 part 无需 content type");
}

#[test]
fn missing_main_document_part_fails_validate() {
    // 根 rels 指向 word/document.xml，但该 part 不存在
    let bytes = build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES_XML.as_bytes()),
        ("_rels/.rels", ROOT_RELS_XML.as_bytes()),
    ]);
    let pkg = open(&bytes);
    assert_eq!(
        pkg.main_document_uri().unwrap().as_str(),
        "word/document.xml"
    );
    let err = pkg.validate().expect_err("主文档缺失应失败");
    assert!(
        matches!(err, OpcError::MissingPart { ref uri } if uri == "word/document.xml"),
        "实际: {err:?}"
    );
}

#[test]
fn missing_content_types_file_fails_open() {
    let bytes = build_zip(&[("_rels/.rels", ROOT_RELS_XML.as_bytes())]);
    let err = Package::from_reader(Cursor::new(&bytes), &PackageLimits::default())
        .expect_err("缺少 [Content_Types].xml");
    assert!(matches!(err, OpcError::Malformed { .. }), "实际: {err:?}");
}

#[test]
fn missing_root_rels_fails_open() {
    let bytes = build_zip(&[("[Content_Types].xml", CONTENT_TYPES_XML.as_bytes())]);
    let err = Package::from_reader(Cursor::new(&bytes), &PackageLimits::default())
        .expect_err("缺少 _rels/.rels");
    assert!(matches!(err, OpcError::Malformed { .. }), "实际: {err:?}");
}

#[test]
fn main_document_uri_resolves_office_document_rel() {
    let pkg = open(&minimal_docx());
    assert_eq!(
        pkg.main_document_uri().unwrap().as_str(),
        "word/document.xml"
    );
    pkg.validate().expect("完整包应通过校验");
}

#[test]
fn main_document_uri_without_office_document_rel() {
    let rels = concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
        r#"<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/>"#,
        "</Relationships>"
    );
    let bytes = build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES_XML.as_bytes()),
        ("_rels/.rels", rels.as_bytes()),
        ("docProps/core.xml", b"<cp:coreProperties/>"),
    ]);
    let pkg = open(&bytes);
    let err = pkg
        .main_document_uri()
        .expect_err("缺 officeDocument 应报 Malformed");
    assert!(matches!(err, OpcError::Malformed { .. }), "实际: {err:?}");
    assert!(matches!(pkg.validate(), Err(OpcError::Malformed { .. })));
}

#[test]
fn external_only_office_document_rel_is_malformed() {
    let rels = concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
        r#"<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="https://example.com/doc" TargetMode="External"/>"#,
        "</Relationships>"
    );
    let bytes = build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES_XML.as_bytes()),
        ("_rels/.rels", rels.as_bytes()),
    ]);
    let pkg = open(&bytes);
    let err = pkg
        .main_document_uri()
        .expect_err("外部 officeDocument 不算主文档");
    assert!(matches!(err, OpcError::Malformed { .. }), "实际: {err:?}");
}
