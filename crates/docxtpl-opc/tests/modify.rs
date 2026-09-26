//! set_part_bytes: only the target part's bytes change; the rest stay the
//! same; derived views are updated in sync.

mod common;

use std::io::Cursor;

use common::*;
use docxtpl_opc::{OpcError, Package, PackageLimits};

fn open(bytes: &[u8]) -> Package {
    Package::from_reader(Cursor::new(bytes), &PackageLimits::default()).expect("open package")
}

#[test]
fn set_part_bytes_changes_only_target() {
    let mut pkg = open(&minimal_docx());
    let before: Vec<(String, Vec<u8>)> = pkg
        .parts()
        .map(|part| (part.name().to_string(), part.bytes().unwrap().to_vec()))
        .collect();
    let new_document = b"<?xml version=\"1.0\"?><w:document><w:body/></w:document>".to_vec();

    pkg.set_part_bytes("word/document.xml", new_document.clone())
        .expect("modify the main document");

    for (name, data) in &before {
        let part = pkg.part(name).expect("part still present");
        if name == "word/document.xml" {
            assert_eq!(part.bytes().unwrap(), new_document);
            assert!(part.is_modified());
        } else {
            assert_eq!(
                part.bytes().unwrap(),
                data,
                "other parts' bytes must not change: {name}"
            );
            assert!(!part.is_modified());
        }
    }

    // Write back and reopen: only that part's bytes change; order is unchanged
    let mut out = Vec::new();
    pkg.write_to(Cursor::new(&mut out)).expect("write back");
    let reopened = open(&out);
    for (name, data) in &before {
        let part = reopened.part(name).expect("part still present");
        if name == "word/document.xml" {
            assert_eq!(part.bytes().unwrap(), new_document);
        } else {
            assert_eq!(
                part.bytes().unwrap(),
                data,
                "writing must not change {name}"
            );
        }
    }
}

#[test]
fn set_part_bytes_missing_part() {
    let mut pkg = open(&minimal_docx());
    let err = pkg
        .set_part_bytes("word/missing.xml", vec![1, 2, 3])
        .expect_err("a nonexistent part should error");
    assert!(
        matches!(err, OpcError::PartNotFound { ref uri } if uri == "word/missing.xml"),
        "got: {err:?}"
    );
}

#[test]
fn set_part_bytes_reloads_rels_and_content_types() {
    let mut pkg = open(&minimal_docx());

    // Modifying word/_rels/document.xml.rels makes relationships_of reflect
    // the new content
    let new_rels = concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
        r#"<Relationship Id="rId9" Type="http://example.com/rel" Target="styles.xml"/>"#,
        "</Relationships>"
    );
    pkg.set_part_bytes("word/_rels/document.xml.rels", new_rels.as_bytes().to_vec())
        .expect("update rels");
    let rels = pkg
        .relationships_of("word/document.xml")
        .expect("the relationships view should update");
    assert_eq!(rels.len(), 1);
    assert!(rels.get("rId9").is_some());
    assert!(rels.get("rId1").is_none());
    pkg.validate()
        .expect("the new rels are valid (they point at word/styles.xml)");

    // Modifying [Content_Types].xml makes content_types() reflect the new
    // content
    let new_ct = concat!(
        "<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">",
        "<Default Extension=\"xml\" ContentType=\"text/plain\"/>",
        "</Types>"
    );
    pkg.set_part_bytes("[Content_Types].xml", new_ct.as_bytes().to_vec())
        .expect("update Content Types");
    let styles_uri = pkg.part("word/styles.xml").unwrap().uri().clone();
    assert_eq!(
        pkg.content_types().content_type_of(&styles_uri),
        Some("text/plain")
    );

    // Modifying _rels/.rels makes root_relationships() reflect the new
    // content
    let new_root = concat!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>"#,
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
        r#"<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>"#,
        "</Relationships>"
    );
    pkg.set_part_bytes("_rels/.rels", new_root.as_bytes().to_vec())
        .expect("update root rels");
    assert_eq!(pkg.root_relationships().len(), 1);
    assert_eq!(
        pkg.main_document_uri().expect("main document URI").as_str(),
        "word/document.xml"
    );
    pkg.validate().expect("the package is still valid");
}

#[test]
fn set_part_bytes_invalid_rels_is_rejected_and_keeps_old_bytes() {
    let mut pkg = open(&minimal_docx());
    // An invalid relationship missing Type/Target
    let bad = concat!(
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
        r#"<Relationship Id="rId1"/>"#,
        "</Relationships>"
    );
    let err = pkg
        .set_part_bytes("word/_rels/document.xml.rels", bad.as_bytes().to_vec())
        .expect_err("invalid rels should fail");
    assert!(
        matches!(err, OpcError::InvalidRelationships { .. }),
        "got: {err:?}"
    );
    // The bytes are unchanged
    assert_eq!(
        pkg.part("word/_rels/document.xml.rels")
            .unwrap()
            .bytes()
            .unwrap(),
        DOCUMENT_RELS_XML.as_bytes()
    );
}
