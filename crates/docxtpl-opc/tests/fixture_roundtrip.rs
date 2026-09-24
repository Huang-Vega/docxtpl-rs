//! P1 验收：真实 fixture 模板（python-docx 生成）经 docxtpl-opc
//! 打开 → validate → 写出 → 重开，part 集合与每个未修改 part 的字节必须完全一致。
//! 对应规划文档 P1：“≥20 个模板无渲染往返通过包校验，未知 part 不丢、关系无悬空”。

use std::fs;
use std::path::PathBuf;

use docxtpl_opc::{Package, PackageLimits};

fn fixtures_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/crates/docxtpl-opc
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("templates")
}

#[test]
fn roundtrip_all_rt_fixtures_byte_identical() {
    let dir = fixtures_dir();
    let mut ids: Vec<String> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("无法读取 fixture 目录 {}: {e}", dir.display()))
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("rt_") && n.ends_with(".docx"))
        .collect();
    ids.sort();

    // P1 验收门槛：至少 20 个往返模板
    assert!(
        ids.len() >= 20,
        "rt_* fixture 数量不足 20（实际 {}），P1 验收不成立",
        ids.len()
    );

    let limits = PackageLimits::default();
    let mut ok = 0usize;
    for name in &ids {
        let path = dir.join(name);
        let pkg = Package::open(&path, &limits).unwrap_or_else(|e| panic!("{name}: 打开失败: {e}"));
        pkg.validate()
            .unwrap_or_else(|e| panic!("{name}: validate 失败（未知 part 丢失/关系悬空）: {e}"));
        assert!(
            pkg.main_document_uri().is_ok(),
            "{name}: 缺少 officeDocument 关系"
        );

        // 记录原始 (part 名, 字节)
        let mut before: Vec<(String, Vec<u8>)> = pkg
            .parts()
            .map(|p| (p.name().to_string(), p.bytes().to_vec()))
            .collect();
        before.sort();

        let tmp = tempfile::NamedTempFile::new().unwrap();
        pkg.save(tmp.path())
            .unwrap_or_else(|e| panic!("{name}: 写出失败: {e}"));

        let reopened =
            Package::open(tmp.path(), &limits).unwrap_or_else(|e| panic!("{name}: 重开失败: {e}"));
        reopened
            .validate()
            .unwrap_or_else(|e| panic!("{name}: 重开后 validate 失败: {e}"));

        let mut after: Vec<(String, Vec<u8>)> = reopened
            .parts()
            .map(|p| (p.name().to_string(), p.bytes().to_vec()))
            .collect();
        after.sort();

        let before_names: Vec<&String> = before.iter().map(|(n, _)| n).collect();
        let after_names: Vec<&String> = after.iter().map(|(n, _)| n).collect();
        assert_eq!(before_names, after_names, "{name}: part 集合不一致");
        for ((bn, bb), (an, ab)) in before.iter().zip(after.iter()) {
            assert_eq!(bn, an);
            assert_eq!(
                bb, ab,
                "{name}: part {bn} 往返后字节变化（未修改 part 必须原样保留）"
            );
        }
        ok += 1;
    }
    eprintln!("P1 往返 fixture 通过：{ok}/{}", ids.len());
}
