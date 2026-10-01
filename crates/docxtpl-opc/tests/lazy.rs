//! Lazy materialization of ZIP parts backed by a path, and raw-copy of
//! unmodified entries.

mod common;

use std::fs;

use common::*;
use docxtpl_opc::{Package, PackageLimits};

fn write_fixture(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("source.docx");
    fs::write(&path, minimal_docx()).expect("write fixture");
    path
}

#[test]
fn path_open_only_eagerly_loads_package_structure() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_fixture(dir.path());
    let pkg = Package::open(&path, &PackageLimits::default()).expect("open package");

    assert!(pkg.part("[Content_Types].xml").unwrap().is_loaded());
    assert!(pkg.part("_rels/.rels").unwrap().is_loaded());
    assert!(pkg
        .part("word/_rels/document.xml.rels")
        .unwrap()
        .is_loaded());
    assert!(!pkg.part("word/document.xml").unwrap().is_loaded());
    assert!(!pkg.part("word/styles.xml").unwrap().is_loaded());

    pkg.validate().expect("validate package");
    assert!(!pkg.part("word/document.xml").unwrap().is_loaded());

    assert_eq!(
        pkg.part("word/document.xml").unwrap().bytes().unwrap(),
        DOCUMENT_XML.as_bytes()
    );
    assert!(pkg.part("word/document.xml").unwrap().is_loaded());
    assert!(!pkg.part("word/styles.xml").unwrap().is_loaded());
}

#[test]
fn saving_unchanged_path_package_does_not_materialize_lazy_parts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_fixture(dir.path());
    let pkg = Package::open(&path, &PackageLimits::default()).expect("open package");
    let output = dir.path().join("copy.docx");

    pkg.save(&output).expect("raw-copy save");
    assert!(!pkg.part("word/document.xml").unwrap().is_loaded());
    assert!(!pkg.part("word/styles.xml").unwrap().is_loaded());

    let reopened = Package::open(&output, &PackageLimits::default()).expect("reopen output");
    reopened.validate().expect("output package is valid");
    assert_eq!(
        reopened.part("word/document.xml").unwrap().bytes().unwrap(),
        DOCUMENT_XML.as_bytes()
    );
}

#[test]
fn write_report_counts_raw_copies_and_final_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_fixture(dir.path());
    let pkg = Package::open(&path, &PackageLimits::default()).expect("open package");
    let output = dir.path().join("reported.docx");

    let report = pkg
        .save_with_report(&output, &docxtpl_opc::WriteOptions::compatible())
        .expect("save with report");

    assert_eq!(report.total_parts, pkg.part_count());
    assert_eq!(report.raw_copied_parts, pkg.part_count());
    assert_eq!(report.rewritten_parts, 0);
    assert_eq!(report.modified_parts, 0);
    assert_eq!(report.output_bytes, fs::metadata(output).unwrap().len());
    assert!(report.raw_copied_compressed_bytes > 0);
    assert!(report.raw_copied_uncompressed_bytes > 0);
}

#[test]
fn reader_backend_remains_eager_when_it_cannot_be_reopened() {
    let pkg = Package::from_reader(
        std::io::Cursor::new(minimal_docx()),
        &PackageLimits::default(),
    )
    .expect("open from reader");

    assert!(pkg.parts().all(|part| part.is_loaded()));
}

#[test]
fn lazy_package_can_replace_its_own_source_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_fixture(dir.path());
    let mut pkg = Package::open(&path, &PackageLimits::default()).expect("open package");
    pkg.set_part_bytes("word/document.xml", b"<w:document/>".to_vec())
        .expect("modify document body");

    pkg.save(&path).expect("overwrite the source file");
    let reopened = Package::open(&path, &PackageLimits::default()).expect("reopen the source file");
    assert_eq!(
        reopened.part("word/document.xml").unwrap().bytes().unwrap(),
        b"<w:document/>"
    );
    assert_eq!(
        reopened.part("word/styles.xml").unwrap().bytes().unwrap(),
        STYLES_XML.as_bytes()
    );
}

#[test]
fn clean_lazy_caches_are_evicted_and_reload_with_identical_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write_fixture(dir.path());
    let mut pkg = Package::open(&path, &PackageLimits::default()).expect("open package");

    let styles_before = pkg
        .part("word/styles.xml")
        .unwrap()
        .bytes()
        .unwrap()
        .to_vec();
    let document_before = pkg
        .part("word/document.xml")
        .unwrap()
        .bytes()
        .unwrap()
        .to_vec();
    pkg.set_part_bytes("word/document.xml", document_before.clone())
        .expect("mark document modified");

    let residency = pkg.residency();
    assert!(residency.resident_bytes >= (styles_before.len() + document_before.len()) as u64);
    assert!(residency.evictable_parts > 0);
    assert!(residency.evictable_bytes >= styles_before.len() as u64);

    let eviction = pkg.evict_clean_part_caches();
    assert_eq!(eviction.before, residency);
    assert_eq!(eviction.evicted_parts, eviction.before.evictable_parts);
    assert_eq!(eviction.evicted_bytes, eviction.before.evictable_bytes);
    assert_eq!(eviction.after.evictable_parts, 0);
    assert_eq!(eviction.after.evictable_bytes, 0);
    assert!(!pkg.part("word/styles.xml").unwrap().is_loaded());
    assert!(
        pkg.part("word/document.xml").unwrap().is_loaded(),
        "modified content must remain resident"
    );

    assert_eq!(
        pkg.part("word/styles.xml").unwrap().bytes().unwrap(),
        styles_before
    );
    assert!(pkg.part("word/styles.xml").unwrap().is_loaded());
    pkg.validate().expect("reloaded package remains valid");
}

#[test]
fn reader_backed_package_has_no_evictable_parts() {
    let mut pkg = Package::from_reader(
        std::io::Cursor::new(minimal_docx()),
        &PackageLimits::default(),
    )
    .expect("open from reader");
    let before = pkg.residency();

    let eviction = pkg.evict_clean_part_caches();

    assert_eq!(before.evictable_parts, 0);
    assert_eq!(eviction.evicted_parts, 0);
    assert_eq!(eviction.evicted_bytes, 0);
    assert_eq!(eviction.before, eviction.after);
}
