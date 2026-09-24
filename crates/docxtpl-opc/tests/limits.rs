//! 输入/输出限额：条目数、单条目解压、总解压、压缩比、输出大小。

mod common;

use std::io::Cursor;

use common::*;
use docxtpl_opc::{OpcError, Package, PackageLimits};

fn open_with(bytes: &[u8], limits: &PackageLimits) -> Result<Package, OpcError> {
    Package::from_reader(Cursor::new(bytes), limits)
}

#[test]
fn entry_count_limit() {
    // 2 个必需条目 + 9 个额外条目 = 11 条
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
    match open_with(&bytes, &limits).expect_err("11 条超 10 条限额") {
        OpcError::LimitExceeded { kind, value, max } => {
            assert_eq!(kind, "entries");
            assert_eq!(value, 11);
            assert_eq!(max, 10);
        }
        other => panic!("期望 LimitExceeded，实际 {other:?}"),
    }

    // 放宽到 11 条即可打开
    let ok_limits = PackageLimits {
        max_entries: 11,
        ..PackageLimits::default()
    };
    open_with(&bytes, &ok_limits).expect("11 条在限额内");
}

#[test]
fn single_entry_uncompressed_limit() {
    // 8MiB 全零数据 deflate 后仅 ~8KB，但声明大小在读取前就被拦截
    let big = vec![0u8; 8 * 1024 * 1024];
    let bytes = build_zip(&[("word/bomb.bin", &big)]);
    let limits = PackageLimits {
        max_entry_uncompressed: 4 * 1024 * 1024,
        ..PackageLimits::default()
    };
    match open_with(&bytes, &limits).expect_err("单条目超限") {
        OpcError::LimitExceeded { kind, value, max } => {
            assert_eq!(kind, "entry_uncompressed");
            assert_eq!(value, 8 * 1024 * 1024);
            assert_eq!(max, 4 * 1024 * 1024);
        }
        other => panic!("期望 LimitExceeded，实际 {other:?}"),
    }
}

#[test]
fn total_uncompressed_limit() {
    // 3MiB + 3MiB 不可压缩数据，总量限 5MiB：第二条累计时超限
    // （不可压缩数据的压缩比 ~1，不会先触发压缩比限额）
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
    match open_with(&bytes, &limits).expect_err("总量超限") {
        OpcError::LimitExceeded { kind, value, max } => {
            assert_eq!(kind, "total_uncompressed");
            assert_eq!(value, 5 * 1024 * 1024 + 1); // 第二条读到剩余预算 +1 即报错
            assert_eq!(max, 5 * 1024 * 1024);
        }
        other => panic!("期望 LimitExceeded，实际 {other:?}"),
    }
    // 放宽总量即可打开（7MiB 覆盖 CT/rels 等条目的额外字节）
    let ok_limits = PackageLimits {
        max_total_uncompressed: 7 * 1024 * 1024,
        ..PackageLimits::default()
    };
    let pkg = open_with(&bytes, &ok_limits).expect("总量在限额内");
    assert_eq!(pkg.part("a.bin").unwrap().bytes().len(), 3 * 1024 * 1024);
    assert_eq!(pkg.part("b.bin").unwrap().bytes().len(), 3 * 1024 * 1024);
}

#[test]
fn compression_ratio_limit() {
    // 真实 8MiB 零数据（deflate 后 ~8KB，压缩比 ~1000），压缩比限 10
    let big = vec![0u8; 8 * 1024 * 1024];
    let bytes = build_zip(&[("bomb.bin", &big)]);
    let limits = PackageLimits {
        max_compression_ratio: 10,
        ..PackageLimits::default()
    };
    match open_with(&bytes, &limits).expect_err("压缩比超限") {
        OpcError::LimitExceeded { kind, value, max } => {
            assert_eq!(kind, "compression_ratio");
            assert_eq!(max, 10);
            assert!(value > 10, "实际压缩比 {value} 应超过限额");
        }
        other => panic!("期望 LimitExceeded，实际 {other:?}"),
    }
    // 8MiB 零数据的压缩比 ~1000，同样超过默认限额（200）：默认也会拦截
    assert!(matches!(
        open_with(&bytes, &PackageLimits::default()),
        Err(OpcError::LimitExceeded {
            kind: "compression_ratio",
            ..
        })
    ));
    // 不可压缩数据（压缩比 ~1）在默认限额下正常
    let plain = incompressible(64 * 1024);
    let ok = build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES_XML.as_bytes()),
        ("_rels/.rels", ROOT_RELS_XML.as_bytes()),
        ("data.bin", &plain),
    ]);
    let pkg = open_with(&ok, &PackageLimits::default()).expect("默认限额内");
    assert_eq!(pkg.part("data.bin").unwrap().bytes(), plain.as_slice());
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
    let pkg = open_with(&bytes, &limits).expect("Stored 条目天然满足压缩比限额");
    assert_eq!(pkg.part("a.bin").unwrap().bytes().len(), 64 * 1024);
}

#[test]
fn output_size_limit() {
    // 16KiB 不可压缩数据：写出必然超过 4KiB
    let blob = incompressible(16 * 1024);
    let bytes = build_zip(&[
        ("[Content_Types].xml", CONTENT_TYPES_XML.as_bytes()),
        ("_rels/.rels", ROOT_RELS_XML.as_bytes()),
        ("word/document.xml", DOCUMENT_XML.as_bytes()),
        ("blob.bin", &blob),
    ]);
    let limits = PackageLimits {
        max_output_size: 4 * 1024,
        ..PackageLimits::default()
    };
    let pkg = open_with(&bytes, &limits).expect("打开不受输出限额影响");

    // write_to
    let err = pkg.write_to(Cursor::new(Vec::new())).expect_err("输出超限");
    match err {
        OpcError::LimitExceeded { kind, value, max } => {
            assert_eq!(kind, "output");
            assert_eq!(max, 4 * 1024);
            assert!(value > 4 * 1024, "尝试写出的总量 {value} 应超限");
        }
        other => panic!("期望 LimitExceeded，实际 {other:?}"),
    }

    // save 同样受限
    let dir = tempfile::tempdir().expect("tempdir");
    let err = pkg
        .save(dir.path().join("x.docx"))
        .expect_err("save 输出超限");
    assert!(
        matches!(err, OpcError::LimitExceeded { kind: "output", .. }),
        "实际: {err:?}"
    );
    // 超限时不应留下半截文件
    assert!(!dir.path().join("x.docx").exists());

    // 默认限额下可以写出且内容往返一致
    let default_pkg = open_with(&bytes, &PackageLimits::default()).expect("默认限额打开");
    let mut out = Vec::new();
    default_pkg
        .write_to(Cursor::new(&mut out))
        .expect("默认限额可写出");
    assert!(out.len() > 4 * 1024);
    let reopened = open_with(&out, &PackageLimits::default()).expect("重开");
    assert_eq!(reopened.part("blob.bin").unwrap().bytes(), blob.as_slice());
}
