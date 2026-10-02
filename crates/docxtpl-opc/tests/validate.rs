//! validate()/main_document_uri(): dangling relationships, external targets,
//! duplicate Ids, and content type coverage.

mod common;

use std::io::Cursor;

use common::*;
use docxtpl_opc::{InterruptibleWriteError, OpcError, Package, PackageLimits};

fn open(bytes: &[u8]) -> Package {
    Package::from_reader(Cursor::new(bytes), &PackageLimits::default()).expect("open package")
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
fn interruptible_validation_stops_inside_package_walk() {
    let pkg = open(&minimal_docx());
    let checks = std::cell::Cell::new(0usize);
    let error = pkg
        .validate_interruptible(&|| {
            let next = checks.get() + 1;
            checks.set(next);
            next >= 3
        })
        .expect_err("validation should observe cancellation");
    assert!(matches!(error, InterruptibleWriteError::Cancelled));
    assert!(checks.get() >= 3);
}

#[test]
fn dangling_internal_relationship_fails_validate() {
    // word/_rels/document.xml.rels points at styles.xml, but the package has
    // no word/styles.xml
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
    // resolve only does path resolution; it does not check existence
    let doc = pkg.part("word/document.xml").unwrap();
    assert_eq!(
        doc.relationships()
            .unwrap()
            .resolve(doc.uri(), "rId1")
            .unwrap()
            .as_str(),
        "word/styles.xml"
    );
    let err = pkg
        .validate()
        .expect_err("a dangling relationship should fail");
    assert!(
        matches!(err, OpcError::InvalidRelationships { .. } if err.to_string().contains("styles.xml")),
        "got: {err:?}"
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
    let err = open(&bytes)
        .validate()
        .expect_err("a target escaping the package root should fail");
    assert!(
        matches!(err, OpcError::InvalidRelationships { .. }),
        "got: {err:?}"
    );
}

#[test]
fn external_relationship_passes_validate() {
    // The document.xml.rels of minimal_docx contains rId2 (External)
    let pkg = open(&minimal_docx());
    let link = pkg
        .relationships_of("word/document.xml")
        .unwrap()
        .get("rId2")
        .unwrap()
        .clone();
    assert_eq!(link.target, "https://example.com/doc");
    assert_eq!(link.target_mode, docxtpl_opc::TargetMode::External);
    pkg.validate()
        .expect("external relationships are excluded from the existence check");
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
    // Both relationships resolve to existing parts, but the Id is duplicated
    let err = pkg.validate().expect_err("a duplicate Id should fail");
    assert!(
        matches!(err, OpcError::InvalidRelationships { .. } if err.to_string().contains("rId1")),
        "got: {err:?}"
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
        .expect_err("a png without a content type should fail");
    assert!(
        matches!(err, OpcError::InvalidContentTypes { .. } if err.to_string().contains("media/image.png")),
        "got: {err:?}"
    );
}

#[test]
fn rels_and_content_types_exempt_from_content_type_check() {
    // .rels and [Content_Types].xml themselves do not require a content type
    // (this package also declares none for png, but it does not error when it
    // contains only exempt parts)
    let bytes = build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES_XML.as_bytes()),
        ("_rels/.rels", ROOT_RELS_XML.as_bytes()),
        ("word/document.xml", DOCUMENT_XML.as_bytes()),
        ("word/_rels/document.xml.rels", EMPTY_RELS_XML.as_bytes()),
    ]);
    open(&bytes)
        .validate()
        .expect("exempt parts need no content type");
}

#[test]
fn missing_main_document_part_fails_validate() {
    // The root rels point at word/document.xml, but the part does not exist
    let bytes = build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES_XML.as_bytes()),
        ("_rels/.rels", ROOT_RELS_XML.as_bytes()),
    ]);
    let pkg = open(&bytes);
    assert_eq!(
        pkg.main_document_uri().unwrap().as_str(),
        "word/document.xml"
    );
    let err = pkg
        .validate()
        .expect_err("a missing main document should fail");
    assert!(
        matches!(err, OpcError::MissingPart { ref uri } if uri == "word/document.xml"),
        "got: {err:?}"
    );
}

#[test]
fn missing_content_types_file_fails_open() {
    let bytes = build_zip(&[("_rels/.rels", ROOT_RELS_XML.as_bytes())]);
    let err = Package::from_reader(Cursor::new(&bytes), &PackageLimits::default())
        .expect_err("missing [Content_Types].xml");
    assert!(matches!(err, OpcError::Malformed { .. }), "got: {err:?}");
}

#[test]
fn missing_root_rels_fails_open() {
    let bytes = build_zip(&[("[Content_Types].xml", CONTENT_TYPES_XML.as_bytes())]);
    let err = Package::from_reader(Cursor::new(&bytes), &PackageLimits::default())
        .expect_err("missing _rels/.rels");
    assert!(matches!(err, OpcError::Malformed { .. }), "got: {err:?}");
}

#[test]
fn main_document_uri_resolves_office_document_rel() {
    let pkg = open(&minimal_docx());
    assert_eq!(
        pkg.main_document_uri().unwrap().as_str(),
        "word/document.xml"
    );
    pkg.validate()
        .expect("a complete package should pass validation");
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
        .expect_err("a missing officeDocument should report Malformed");
    assert!(matches!(err, OpcError::Malformed { .. }), "got: {err:?}");
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
        .expect_err("an external officeDocument does not count as the main document");
    assert!(matches!(err, OpcError::Malformed { .. }), "got: {err:?}");
}
