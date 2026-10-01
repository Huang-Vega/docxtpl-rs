//! Input/output limits: entry count, per-entry uncompressed size, total
//! uncompressed size, compression ratio, and output size.

mod common;

use std::io::Cursor;

use common::*;
use docxtpl_opc::{OpcError, Package, PackageLimits};

fn open_with(bytes: &[u8], limits: &PackageLimits) -> Result<Package, OpcError> {
    Package::from_reader(Cursor::new(bytes), limits)
}

#[test]
fn entry_count_limit() {
    // 2 required entries + 9 extra entries = 11 entries
    let mut entries: Vec<(String, Vec<u8>)> = vec![
        (
            "[Content_Types].xml".to_string(),
            CONTENT_TYPES_XML.as_bytes().to_vec(),
        ),
        ("_rels/.rels".to_string(), ROOT_RELS_XML.as_bytes().to_vec()),
    ];
    for i in 0..9 {
        entries.push((format!("word/part{i}.xml"), b"<x/>".to_vec()));
    }
    let refs: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(name, data)| (name.as_str(), data.as_slice()))
        .collect();
    let bytes = build_zip(&refs);

    let limits = PackageLimits {
        max_entries: 10,
        ..PackageLimits::default()
    };
    match open_with(&bytes, &limits).expect_err("11 entries exceed the limit of 10") {
        OpcError::LimitExceeded { kind, value, max } => {
            assert_eq!(kind, "entries");
            assert_eq!(value, 11);
            assert_eq!(max, 10);
        }
        other => panic!("expected LimitExceeded, got {other:?}"),
    }

    // Relaxing to 11 entries makes it open
    let ok_limits = PackageLimits {
        max_entries: 11,
        ..PackageLimits::default()
    };
    open_with(&bytes, &ok_limits).expect("11 entries are within the limit");
}

#[test]
fn single_entry_uncompressed_limit() {
    // 8MiB of all-zero data deflates to ~8KB, but the declared size is blocked
    // before reading
    let big = vec![0u8; 8 * 1024 * 1024];
    let bytes = build_zip(&[("word/bomb.bin", &big)]);
    let limits = PackageLimits {
        max_entry_uncompressed: 4 * 1024 * 1024,
        ..PackageLimits::default()
    };
    match open_with(&bytes, &limits).expect_err("single entry exceeds the limit") {
        OpcError::LimitExceeded { kind, value, max } => {
            assert_eq!(kind, "entry_uncompressed");
            assert_eq!(value, 8 * 1024 * 1024);
            assert_eq!(max, 4 * 1024 * 1024);
        }
        other => panic!("expected LimitExceeded, got {other:?}"),
    }
}

#[test]
fn total_uncompressed_limit() {
    // 3MiB + 3MiB of incompressible data with a 5MiB total limit: the limit is
    // hit while accumulating the second entry (incompressible data has a ratio
    // of ~1, so it cannot trip the ratio limit first)
    let big = incompressible(3 * 1024 * 1024);
    let bytes = build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES_XML.as_bytes()),
        ("_rels/.rels", ROOT_RELS_XML.as_bytes()),
        ("a.bin", &big),
        ("b.bin", &big),
    ]);
    let limits = PackageLimits {
        max_total_uncompressed: 5 * 1024 * 1024,
        ..PackageLimits::default()
    };
    match open_with(&bytes, &limits).expect_err("total size exceeds the limit") {
        OpcError::LimitExceeded { kind, value, max } => {
            assert_eq!(kind, "total_uncompressed");
            assert_eq!(value, 5 * 1024 * 1024 + 1); // Errors after reading the remaining budget +1
            assert_eq!(max, 5 * 1024 * 1024);
        }
        other => panic!("expected LimitExceeded, got {other:?}"),
    }
    // Relaxing the total limit allows opening (7MiB covers the extra bytes of
    // CT/rels entries)
    let ok_limits = PackageLimits {
        max_total_uncompressed: 7 * 1024 * 1024,
        ..PackageLimits::default()
    };
    let pkg = open_with(&bytes, &ok_limits).expect("total size is within the limit");
    assert_eq!(
        pkg.part("a.bin").unwrap().bytes().unwrap().len(),
        3 * 1024 * 1024
    );
    assert_eq!(
        pkg.part("b.bin").unwrap().bytes().unwrap().len(),
        3 * 1024 * 1024
    );
}

#[test]
fn compression_ratio_limit() {
    // Real 8MiB of zero data (~8KB after deflate, ratio ~1000) with a ratio
    // limit of 10
    let big = vec![0u8; 8 * 1024 * 1024];
    let bytes = build_zip(&[("bomb.bin", &big)]);
    let limits = PackageLimits {
        max_compression_ratio: 10,
        ..PackageLimits::default()
    };
    match open_with(&bytes, &limits).expect_err("compression ratio exceeds the limit") {
        OpcError::LimitExceeded { kind, value, max } => {
            assert_eq!(kind, "compression_ratio");
            assert_eq!(max, 10);
            assert!(value > 10, "actual ratio {value} should exceed the limit");
        }
        other => panic!("expected LimitExceeded, got {other:?}"),
    }
    // 8MiB of zero data has a ratio of ~1000, which also exceeds the default
    // limit (200): the defaults block it too
    assert!(matches!(
        open_with(&bytes, &PackageLimits::default()),
        Err(OpcError::LimitExceeded {
            kind: "compression_ratio",
            ..
        })
    ));
    // Incompressible data (ratio ~1) works under the default limits
    let plain = incompressible(64 * 1024);
    let ok = build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES_XML.as_bytes()),
        ("_rels/.rels", ROOT_RELS_XML.as_bytes()),
        ("data.bin", &plain),
    ]);
    let pkg = open_with(&ok, &PackageLimits::default()).expect("within default limits");
    assert_eq!(
        pkg.part("data.bin").unwrap().bytes().unwrap(),
        plain.as_slice()
    );
}

#[test]
fn stored_entries_are_exempt_from_ratio_limit() {
    let data = vec![0u8; 64 * 1024];
    let bytes = raw_stored_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES_XML.as_bytes()),
        ("_rels/.rels", ROOT_RELS_XML.as_bytes()),
        ("a.bin", &data),
    ]);
    let limits = PackageLimits {
        max_compression_ratio: 2,
        ..PackageLimits::default()
    };
    let pkg =
        open_with(&bytes, &limits).expect("Stored entries inherently satisfy the ratio limit");
    assert_eq!(pkg.part("a.bin").unwrap().bytes().unwrap().len(), 64 * 1024);
}

#[test]
fn output_size_limit() {
    // 16KiB of incompressible data: writing it must exceed 4KiB
    let blob = incompressible(16 * 1024);
    let content_types = CONTENT_TYPES_XML.replace(
        "</Types>",
        r#"<Default Extension="bin" ContentType="application/octet-stream"/></Types>"#,
    );
    let bytes = build_zip(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", ROOT_RELS_XML.as_bytes()),
        ("word/document.xml", DOCUMENT_XML.as_bytes()),
        ("blob.bin", &blob),
    ]);
    let limits = PackageLimits {
        max_output_size: 4 * 1024,
        ..PackageLimits::default()
    };
    let pkg = open_with(&bytes, &limits).expect("opening is unaffected by the output limit");

    // write_to
    let err = pkg
        .write_to(Cursor::new(Vec::new()))
        .expect_err("output exceeds the limit");
    match err {
        OpcError::LimitExceeded { kind, value, max } => {
            assert_eq!(kind, "output");
            assert_eq!(max, 4 * 1024);
            assert!(
                value > 4 * 1024,
                "attempted output total {value} should exceed the limit"
            );
        }
        other => panic!("expected LimitExceeded, got {other:?}"),
    }

    // save is limited in the same way
    let dir = tempfile::tempdir().expect("tempdir");
    let err = pkg
        .save(dir.path().join("x.docx"))
        .expect_err("save output exceeds the limit");
    assert!(
        matches!(err, OpcError::LimitExceeded { kind: "output", .. }),
        "got: {err:?}"
    );
    // No truncated file should be left when the limit is exceeded
    assert!(!dir.path().join("x.docx").exists());

    // An existing destination is also preserved: save never truncates it
    // before validation and bounded ZIP serialization succeed.
    let existing = dir.path().join("existing.docx");
    let sentinel = b"previous valid output";
    std::fs::write(&existing, sentinel).expect("write existing target");
    let err = pkg
        .save(&existing)
        .expect_err("limited save over existing target must fail");
    assert!(matches!(
        err,
        OpcError::LimitExceeded { kind: "output", .. }
    ));
    assert_eq!(
        std::fs::read(&existing).expect("read preserved target"),
        sentinel
    );

    // Under the default limits it writes successfully and round-trips
    let default_pkg =
        open_with(&bytes, &PackageLimits::default()).expect("open with default limits");
    let mut out = Vec::new();
    default_pkg
        .write_to(Cursor::new(&mut out))
        .expect("write with default limits");
    assert!(out.len() > 4 * 1024);
    let reopened = open_with(&out, &PackageLimits::default()).expect("reopen");
    assert_eq!(
        reopened.part("blob.bin").unwrap().bytes().unwrap(),
        blob.as_slice()
    );
}

#[test]
fn mutations_obey_entry_and_uncompressed_limits() {
    let bytes = build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES_XML.as_bytes()),
        ("_rels/.rels", ROOT_RELS_XML.as_bytes()),
        ("word/document.xml", DOCUMENT_XML.as_bytes()),
    ]);
    let existing_total =
        (CONTENT_TYPES_XML.len() + ROOT_RELS_XML.len() + DOCUMENT_XML.len()) as u64;

    let entry_limits = PackageLimits {
        max_entries: 3,
        ..PackageLimits::default()
    };
    let mut pkg = open_with(&bytes, &entry_limits).expect("open at entry limit");
    assert!(matches!(
        pkg.add_part("extra.xml", Vec::new()),
        Err(OpcError::LimitExceeded {
            kind: "entries",
            ..
        })
    ));

    let single_limits = PackageLimits {
        max_entry_uncompressed: 1024,
        ..PackageLimits::default()
    };
    let mut pkg = open_with(&bytes, &single_limits).expect("open below single-entry limit");
    assert!(matches!(
        pkg.set_part_bytes("word/document.xml", vec![0; 1025]),
        Err(OpcError::LimitExceeded {
            kind: "entry_uncompressed",
            ..
        })
    ));

    let total_limits = PackageLimits {
        max_total_uncompressed: existing_total + 10,
        ..PackageLimits::default()
    };
    let mut pkg = open_with(&bytes, &total_limits).expect("open below total limit");
    assert!(matches!(
        pkg.add_part("extra.xml", vec![0; 11]),
        Err(OpcError::LimitExceeded {
            kind: "total_uncompressed",
            ..
        })
    ));
}
