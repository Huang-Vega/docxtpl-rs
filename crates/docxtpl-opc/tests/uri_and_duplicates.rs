//! URI validation (at the ZIP layer) and three-scope duplicate entry
//! detection.

mod common;

use std::io::Cursor;

use common::*;
use docxtpl_opc::{OpcError, Package, PackageLimits};

fn try_open(bytes: &[u8]) -> Result<Package, OpcError> {
    Package::from_reader(Cursor::new(bytes), &PackageLimits::default())
}

#[test]
fn rejects_traversal_and_invalid_entry_names() {
    for bad in [
        "../evil.txt",
        "a/../b.txt",
        "..",
        "/abs.txt",
        "a\\b.txt",
        "C:\\x.txt",
        "C:x.txt",
        "a/./b.txt",
    ] {
        let bytes = build_zip(&[(bad, b"x")]);
        let err = try_open(&bytes).expect_err(bad);
        assert!(
            matches!(err, OpcError::InvalidUri { ref uri, .. } if uri == bad),
            "expected {bad} to report InvalidUri, got {err:?}"
        );
    }
}

#[test]
fn rejects_empty_entry_name() {
    // ZipWriter cannot easily build an empty name, so use a handcrafted ZIP
    let bytes = raw_stored_zip(&[("", b"x")]);
    let err = try_open(&bytes).expect_err("an empty entry name should be rejected");
    assert!(matches!(err, OpcError::InvalidUri { .. }), "got: {err:?}");
}

#[test]
fn accepts_unusual_but_valid_entry_names() {
    // Good examples (the unit tests have at least 15 more good/bad table
    // cases): empty segments, directory entries, non-drive colons, etc.
    for good in [
        "word/document.xml",
        "a/b.xml",
        "customXml/item1.xml",
        "a//b.xml",
        "dir/",
        "a/b:c",
        "docProps/core.xml",
    ] {
        let bytes = build_zip(&[
            ("[Content_Types].xml", CONTENT_TYPES_XML.as_bytes()),
            ("_rels/.rels", ROOT_RELS_XML.as_bytes()),
            (good, b"x"),
        ]);
        let pkg = try_open(&bytes).unwrap_or_else(|err| panic!("{good} should open: {err:?}"));
        assert!(pkg.contains(good));
    }
}

#[test]
fn detects_exact_duplicate() {
    // ZipWriter refuses to write duplicate names, so use a handcrafted ZIP
    let bytes = raw_stored_zip(&[("a.xml", b"1"), ("b.txt", b"2"), ("a.xml", b"3")]);
    let err = try_open(&bytes).expect_err("an exact duplicate should be rejected");
    assert!(
        matches!(err, OpcError::DuplicateEntry { ref uri, scope: "exact" } if uri == "a.xml"),
        "got: {err:?}"
    );
}

#[test]
fn detects_case_duplicate() {
    let bytes = build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES_XML.as_bytes()),
        ("_rels/.rels", ROOT_RELS_XML.as_bytes()),
        ("word/document.xml", DOCUMENT_XML.as_bytes()),
        ("word/Document.XML", DOCUMENT_XML.as_bytes()),
    ]);
    let err = try_open(&bytes).expect_err("a case-folded duplicate should be rejected");
    assert!(
        matches!(err, OpcError::DuplicateEntry { scope: "case", .. }),
        "got: {err:?}"
    );
}

#[test]
fn detects_percent_duplicate() {
    let bytes = build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES_XML.as_bytes()),
        ("_rels/.rels", ROOT_RELS_XML.as_bytes()),
        ("a b.xml", b"1"),
        ("a%20b.xml", b"2"),
    ]);
    let err = try_open(&bytes).expect_err("a percent-decode-folded duplicate should be rejected");
    assert!(
        matches!(
            err,
            OpcError::DuplicateEntry {
                scope: "percent",
                ..
            }
        ),
        "got: {err:?}"
    );
}

#[test]
fn double_slash_names_are_allowed_but_conflict_with_collapsed() {
    // a//b on its own is valid
    let ok = build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES_XML.as_bytes()),
        ("_rels/.rels", ROOT_RELS_XML.as_bytes()),
        ("a//b.xml", b"1"),
    ]);
    let pkg = try_open(&ok).expect("a//b.xml is valid on its own");
    assert!(pkg.contains("a//b.xml"));
    assert_eq!(pkg.part("a//b.xml").unwrap().uri().file_name(), "b.xml");
    assert_eq!(
        pkg.part("a//b.xml")
            .unwrap()
            .uri()
            .parent()
            .unwrap()
            .as_str(),
        "a"
    );

    // a//b coexisting with a/b conflicts under the normalization (exact)
    // scope
    let conflict = build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES_XML.as_bytes()),
        ("_rels/.rels", ROOT_RELS_XML.as_bytes()),
        ("a//b.xml", b"1"),
        ("a/b.xml", b"2"),
    ]);
    let err = try_open(&conflict)
        .expect_err("a duplicate after empty-segment collapse should be rejected");
    assert!(
        matches!(err, OpcError::DuplicateEntry { scope: "exact", .. }),
        "got: {err:?}"
    );
}
