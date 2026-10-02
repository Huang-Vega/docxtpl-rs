//! set_part_bytes: only the target part's bytes change; the rest stay the
//! same; derived views are updated in sync.

mod common;

use std::cell::Cell;
use std::io::Cursor;
use std::sync::Arc;

use common::*;
use docxtpl_opc::{FilePartSource, InterruptibleWriteError, OpcError, Package, PackageLimits};

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
fn add_shared_part_retains_arc_allocation_and_honors_limits() {
    let package_bytes = minimal_docx();
    let baseline = open(&package_bytes);
    let baseline_len: u64 = baseline
        .parts()
        .map(|part| part.bytes().unwrap().len() as u64)
        .sum();
    let shared: Arc<[u8]> = Arc::from(&b"shared image bytes"[..]);
    let shared_ptr = shared.as_ptr();
    let limits = PackageLimits {
        max_total_uncompressed: baseline_len + shared.len() as u64,
        ..PackageLimits::default()
    };
    let mut pkg = Package::from_reader(Cursor::new(&package_bytes), &limits).expect("open package");

    pkg.add_shared_part("word/media/image1.bin", Arc::clone(&shared))
        .expect("add shared part");
    let part = pkg.part("word/media/image1.bin").unwrap();
    assert_eq!(part.bytes().unwrap().as_ptr(), shared_ptr);
    assert!(part.is_loaded());

    let tight_limits = PackageLimits {
        max_total_uncompressed: baseline_len + shared.len() as u64 - 1,
        ..PackageLimits::default()
    };
    let mut tight =
        Package::from_reader(Cursor::new(&package_bytes), &tight_limits).expect("open package");
    let error = tight
        .add_shared_part("word/media/image1.bin", shared)
        .expect_err("shared part must obey total size limit");
    assert!(matches!(error, OpcError::LimitExceeded { .. }));
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

#[test]
fn set_file_backed_part_streams_replacement() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_path = dir.path().join("document.xml");
    let replacement = b"<w:document><w:body><w:p/></w:body></w:document>";
    std::fs::write(&source_path, replacement).expect("write replacement");
    let source = FilePartSource::snapshot(&source_path).expect("snapshot replacement");
    let mut pkg = open(&minimal_docx());

    pkg.set_file_backed_part("word/document.xml", source)
        .expect("set file-backed document");

    let part = pkg.part("word/document.xml").unwrap();
    assert!(part.is_modified());
    assert!(!part.is_loaded(), "replacement should stay file-backed");

    let mut out = Vec::new();
    pkg.write_to(Cursor::new(&mut out))
        .expect("stream file-backed package");
    assert!(
        !pkg.part("word/document.xml").unwrap().is_loaded(),
        "streaming must not populate the in-memory cache"
    );
    let reopened = open(&out);
    assert_eq!(
        reopened.part("word/document.xml").unwrap().bytes().unwrap(),
        replacement
    );
}

#[test]
fn write_report_counts_modified_file_backed_replacement() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_path = dir.path().join("document.xml");
    let replacement = b"<w:document><w:body/></w:document>";
    std::fs::write(&source_path, replacement).expect("write replacement");
    let source = FilePartSource::snapshot(&source_path).expect("snapshot replacement");
    let mut pkg = open(&minimal_docx());
    pkg.set_file_backed_part("word/document.xml", source)
        .expect("set file-backed document");

    let mut out = Vec::new();
    let report = pkg
        .write_to_with_report(
            Cursor::new(&mut out),
            &docxtpl_opc::WriteOptions::compatible(),
        )
        .expect("write with report");

    assert_eq!(report.total_parts, pkg.part_count());
    assert_eq!(
        report.raw_copied_parts, 0,
        "reader-backed input has no raw source"
    );
    assert_eq!(report.rewritten_parts, pkg.part_count());
    assert_eq!(report.file_backed_parts, 1);
    assert_eq!(report.modified_parts, 1);
    assert_eq!(report.output_bytes, out.len() as u64);
    assert_eq!(report.temporary_file_bytes, 0);
    assert_eq!(report.temporary_sync_elapsed, std::time::Duration::ZERO);
    assert_eq!(report.atomic_replace_elapsed, std::time::Duration::ZERO);
    assert!(report.rewritten_source_bytes >= replacement.len() as u64);
    assert!(!pkg.part("word/document.xml").unwrap().is_loaded());
}

#[test]
fn interruptible_write_stops_with_a_distinct_error() {
    let pkg = open(&minimal_docx());
    let checks = Cell::new(0usize);
    let should_cancel = || {
        let next = checks.get() + 1;
        checks.set(next);
        next >= 3
    };
    let mut out = Vec::new();

    let error = pkg
        .write_to_with_report_interruptible(
            Cursor::new(&mut out),
            &docxtpl_opc::WriteOptions::compatible(),
            &should_cancel,
        )
        .expect_err("write should stop after cancellation");

    assert!(matches!(error, InterruptibleWriteError::Cancelled));
    assert!(checks.get() >= 3);
}

#[test]
fn cancelled_atomic_save_preserves_existing_destination() {
    let pkg = open(&minimal_docx());
    let directory = tempfile::tempdir().expect("tempdir");
    let destination = directory.path().join("output.docx");
    std::fs::write(&destination, b"preserved").expect("write old destination");

    let error = pkg
        .save_with_report_interruptible(
            &destination,
            &docxtpl_opc::WriteOptions::compatible(),
            &|| true,
        )
        .expect_err("pre-cancelled save should fail");

    assert!(matches!(error, InterruptibleWriteError::Cancelled));
    assert_eq!(std::fs::read(destination).unwrap(), b"preserved");
}

#[test]
fn file_backed_part_rejects_changed_source() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source_path = dir.path().join("document.xml");
    std::fs::write(&source_path, DOCUMENT_XML).expect("write replacement");
    let source = FilePartSource::snapshot(&source_path).expect("snapshot replacement");
    let mut pkg = open(&minimal_docx());
    pkg.set_file_backed_part("word/document.xml", source)
        .expect("set file-backed document");
    std::fs::write(&source_path, b"changed").expect("change replacement");

    let mut out = Vec::new();
    let err = pkg
        .write_to(Cursor::new(&mut out))
        .expect_err("changed source must fail");
    assert!(err.to_string().contains("source changed"), "got: {err:?}");
}

#[test]
fn file_part_snapshot_rejects_directories() {
    let dir = tempfile::tempdir().expect("tempdir");
    let err = FilePartSource::snapshot(dir.path()).expect_err("directory is not a file");
    assert!(err.to_string().contains("not a file"), "got: {err:?}");
}

#[test]
fn package_transaction_rolls_back_touched_and_added_parts_on_drop() {
    let mut pkg = open(&minimal_docx());
    let original = pkg
        .part("word/document.xml")
        .unwrap()
        .bytes()
        .unwrap()
        .to_vec();
    {
        let mut transaction = pkg.transaction();
        transaction
            .set_part_bytes("word/document.xml", b"changed".to_vec())
            .expect("replace document");
        transaction
            .add_part("word/temporary.bin", vec![1, 2, 3])
            .expect("add temporary part");
        assert!(transaction.changed());
        assert_eq!(
            transaction.touched_parts(),
            vec![
                "word/document.xml".to_string(),
                "word/temporary.bin".to_string()
            ]
        );
    }

    assert_eq!(
        pkg.part("word/document.xml").unwrap().bytes().unwrap(),
        original
    );
    assert!(!pkg.contains("word/temporary.bin"));
    pkg.validate().expect("rolled-back package is valid");
}

#[test]
fn package_transaction_commit_keeps_changes() {
    let mut pkg = open(&minimal_docx());
    let changed = b"<w:document><w:body/></w:document>";
    {
        let mut transaction = pkg.transaction();
        transaction
            .set_part_bytes("word/document.xml", changed.to_vec())
            .expect("replace document");
        transaction.commit();
    }

    assert_eq!(
        pkg.part("word/document.xml").unwrap().bytes().unwrap(),
        changed
    );
}

#[test]
fn package_transaction_reports_journal_sizes_without_loading_untouched_parts() {
    let mut pkg = open(&minimal_docx());
    let original_len = pkg
        .part("word/document.xml")
        .expect("document part")
        .bytes()
        .expect("document bytes")
        .len() as u64;
    let mut transaction = pkg.transaction();
    transaction
        .set_part_bytes("word/document.xml", b"replacement".to_vec())
        .expect("replace part");
    transaction
        .add_part("word/added.bin", vec![1, 2, 3, 4])
        .expect("add part");

    let metrics = transaction.metrics();
    assert_eq!(metrics.snapshotted_parts, 1);
    assert_eq!(metrics.added_parts, 1);
    assert_eq!(metrics.snapshotted_bytes, original_len);
    assert_eq!(metrics.current_touched_bytes, 15);
    assert_eq!(transaction.residency(), transaction.package().residency());

    transaction.rollback();
    assert!(pkg.part("word/added.bin").is_none());
}

#[test]
fn package_transaction_restores_relationship_derived_views() {
    let mut pkg = open(&minimal_docx());
    let original_id = pkg
        .relationships_of("word/document.xml")
        .unwrap()
        .iter()
        .next()
        .unwrap()
        .id
        .clone();
    {
        let mut transaction = pkg.transaction();
        let changed_rels = concat!(
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
            r#"<Relationship Id="rId9" Type="http://example.com/rel" Target="styles.xml"/>"#,
            "</Relationships>"
        );
        transaction
            .set_part_bytes(
                "word/_rels/document.xml.rels",
                changed_rels.as_bytes().to_vec(),
            )
            .expect("replace relationships");
        transaction.rollback();
    }

    let relationships = pkg.relationships_of("word/document.xml").unwrap();
    assert!(relationships.get(&original_id).is_some());
    assert!(relationships.get("rId9").is_none());
    pkg.validate().expect("rolled-back package is valid");
}

#[test]
fn validation_failure_does_not_replace_existing_output() {
    let mut pkg = open(&minimal_docx());
    let dangling_rels = concat!(
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
        r#"<Relationship Id="rId1" Type="http://example.com/missing" Target="missing.xml"/>"#,
        "</Relationships>"
    );
    pkg.set_part_bytes(
        "word/_rels/document.xml.rels",
        dangling_rels.as_bytes().to_vec(),
    )
    .expect("install syntactically valid dangling relationship");
    let dir = tempfile::tempdir().expect("tempdir");
    let output = dir.path().join("output.docx");
    let sentinel = b"previous valid output";
    std::fs::write(&output, sentinel).expect("write existing target");

    let error = pkg
        .save(&output)
        .expect_err("final validation must reject dangling relationship");

    assert!(error.to_string().contains("does not match any part"));
    assert_eq!(std::fs::read(output).unwrap(), sentinel);
}
