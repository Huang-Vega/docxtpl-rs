//! URI 校验（ZIP 层面）与重复条目三口径检测。

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
            "期望 {bad} 报 InvalidUri，实际 {err:?}"
        );
    }
}

#[test]
fn rejects_empty_entry_name() {
    // ZipWriter 不便构造空名，用手工 ZIP
    let bytes = raw_stored_zip(&[("", b"x")]);
    let err = try_open(&bytes).expect_err("空条目名应被拒绝");
    assert!(matches!(err, OpcError::InvalidUri { .. }), "实际: {err:?}");
}

#[test]
fn accepts_unusual_but_valid_entry_names() {
    // 好样例（单元测试中另有 ≥15 组好/坏表）：空段、目录条目、非盘符冒号等
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
        let pkg = try_open(&bytes).unwrap_or_else(|err| panic!("{good} 应可打开: {err:?}"));
        assert!(pkg.contains(good));
    }
}

#[test]
fn detects_exact_duplicate() {
    // ZipWriter 拒绝写入重复名，用手工 ZIP
    let bytes = raw_stored_zip(&[("a.xml", b"1"), ("b.txt", b"2"), ("a.xml", b"3")]);
    let err = try_open(&bytes).expect_err("精确重复应被拒绝");
    assert!(
        matches!(err, OpcError::DuplicateEntry { ref uri, scope: "exact" } if uri == "a.xml"),
        "实际: {err:?}"
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
    let err = try_open(&bytes).expect_err("大小写折叠重复应被拒绝");
    assert!(
        matches!(err, OpcError::DuplicateEntry { scope: "case", .. }),
        "实际: {err:?}"
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
    let err = try_open(&bytes).expect_err("百分号解码折叠重复应被拒绝");
    assert!(
        matches!(
            err,
            OpcError::DuplicateEntry {
                scope: "percent",
                ..
            }
        ),
        "实际: {err:?}"
    );
}

#[test]
fn double_slash_names_are_allowed_but_conflict_with_collapsed() {
    // a//b 单独存在合法
    let ok = build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES_XML.as_bytes()),
        ("_rels/.rels", ROOT_RELS_XML.as_bytes()),
        ("a//b.xml", b"1"),
    ]);
    let pkg = try_open(&ok).expect("a//b.xml 本身合法");
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

    // a//b 与 a/b 并存 → 规范化口径（exact）冲突
    let conflict = build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES_XML.as_bytes()),
        ("_rels/.rels", ROOT_RELS_XML.as_bytes()),
        ("a//b.xml", b"1"),
        ("a/b.xml", b"2"),
    ]);
    let err = try_open(&conflict).expect_err("空段折叠后重复应被拒绝");
    assert!(
        matches!(err, OpcError::DuplicateEntry { scope: "exact", .. }),
        "实际: {err:?}"
    );
}
