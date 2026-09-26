//! Synthetic docx round trip: open → assert → write back → reopen, with
//! unmodified part bytes matching one by one.

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
    Package::from_reader(Cursor::new(bytes), &PackageLimits::default()).expect("open package")
}

#[test]
fn minimal_docx_roundtrip() {
    let original = minimal_docx();
    let pkg = open(&original);

    // Part set and order
    let names: Vec<&str> = pkg.parts().map(|part| part.name()).collect();
    assert_eq!(names, EXPECTED_NAMES);
    assert_eq!(pkg.part_count(), 5);
    assert!(pkg.contains("word/document.xml"));
    assert!(pkg.contains("[Content_Types].xml"));
    assert!(!pkg.contains("word/missing.xml"));
    assert!(pkg.parts().all(|part| !part.is_modified()));
    assert!(pkg.parts().all(|part| !part.is_dir()));

    // Relationships of the part
    let doc = pkg.part("word/document.xml").expect("main document exists");
    assert_eq!(doc.uri().as_str(), "word/document.xml");
    assert_eq!(doc.name(), "word/document.xml");
    let rels = doc
        .relationships()
        .expect("main document should have relationships");
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

    // resolve: the target is normalized relative to the part's directory
    let doc_uri = doc.uri().clone();
    assert_eq!(
        rels.resolve(&doc_uri, "rId1").expect("resolve").as_str(),
        "word/styles.xml"
    );
    // External relationships do not participate in in-package resolution
    assert!(rels.resolve(&doc_uri, "rId2").is_none());
    assert!(rels.resolve(&doc_uri, "missing").is_none());
    // Keep the original file order
    let ids: Vec<&str> = rels.iter().map(|rel| rel.id.as_str()).collect();
    assert_eq!(ids, ["rId1", "rId2"]);
    assert_eq!(
        pkg.relationships_of("word/document.xml")
            .map(Relationships::len),
        Some(2)
    );
    assert!(pkg.relationships_of("word/styles.xml").is_none());

    // Root relationships and the main document
    assert_eq!(pkg.root_relationships().len(), 1);
    assert_eq!(
        pkg.main_document_uri().expect("main document URI").as_str(),
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

    // Integrity
    pkg.validate().expect("minimal docx should pass validation");

    // Write back (in-memory stream)
    let mut out = Vec::new();
    pkg.write_to(Cursor::new(&mut out)).expect("write_to");
    let reopened = open(&out);
    let names_after: Vec<&str> = reopened.parts().map(|part| part.name()).collect();
    assert_eq!(
        names_after, EXPECTED_NAMES,
        "entry order is unchanged after write"
    );
    for part in pkg.parts() {
        assert_eq!(
            part.bytes().unwrap(),
            reopened
                .part(part.name())
                .expect("reopened package should have the same part")
                .bytes()
                .unwrap(),
            "part {} was not modified; bytes should match one by one",
            part.name()
        );
    }

    // Save to disk and reopen
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("out.docx");
    pkg.save(&path).expect("save");
    let from_disk = Package::open(&path, &PackageLimits::default()).expect("reopen");
    assert_eq!(from_disk.part_count(), 5);
    for part in pkg.parts() {
        assert_eq!(
            part.bytes().unwrap(),
            from_disk
                .part(part.name())
                .expect("part is still present")
                .bytes()
                .unwrap()
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

    let dir_part = pkg
        .part("word/")
        .expect("directory entry should be preserved");
    assert!(dir_part.is_dir());
    assert!(dir_part.bytes().unwrap().is_empty());
    assert!(dir_part.uri().parent().is_none());
    assert_eq!(dir_part.uri().file_name(), "word");
    assert!(dir_part.relationships().is_none());

    // Empty rels still form a relationships view (an empty set)
    let doc = pkg.part("word/document.xml").expect("main document exists");
    assert_eq!(doc.relationships().map(Relationships::len), Some(0));

    pkg.validate()
        .expect("a package with directory entries should pass validation");

    let mut out = Vec::new();
    pkg.write_to(Cursor::new(&mut out)).expect("write back");
    let reopened = open(&out);
    let dir_after = reopened
        .part("word/")
        .expect("directory entry should be written back");
    assert!(dir_after.is_dir());
    assert!(dir_after.bytes().unwrap().is_empty());
    assert_eq!(
        reopened
            .part("word/document.xml")
            .expect("main document still present")
            .bytes()
            .unwrap(),
        DOCUMENT_XML.as_bytes()
    );
}
