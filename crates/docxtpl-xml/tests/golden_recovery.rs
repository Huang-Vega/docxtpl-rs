//! Golden：tests/fixtures/stages/ 下全部 <id>.pre_recover.xml /
//! <id>.recovered.xml 成对校验。
//!
//! 宽松解析 pre_recover 得到的树，必须与严格解析 recovered 得到的树
//! 结构完全相等（标签、属性序列、xmlns 声明、子节点与文本）。

mod common;

use std::path::PathBuf;

use common::trees_equal;
use docxtpl_xml::{XmlDocument, XmlLimits};

fn stages_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("stages")
}

#[test]
fn golden_recovery_all_pairs() {
    let dir = stages_dir();
    assert!(dir.is_dir(), "找不到 stages 目录: {}", dir.display());
    let mut ids: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("读取 stages 目录") {
        let entry = entry.expect("读取目录项");
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(id) = name.strip_suffix(".pre_recover.xml") {
            ids.push(id.to_string());
        }
    }
    ids.sort();
    assert!(!ids.is_empty(), "未枚举到任何 pre_recover fixture");

    let limits = XmlLimits::default();
    let mut failures: Vec<String> = Vec::new();
    let mut checked = 0usize;
    for id in &ids {
        let pre_path = dir.join(format!("{id}.pre_recover.xml"));
        let recovered_path = dir.join(format!("{id}.recovered.xml"));
        if !recovered_path.is_file() {
            failures.push(format!("{id}: 缺少配对的 recovered.xml"));
            continue;
        }
        let pre = std::fs::read_to_string(&pre_path).expect("读 pre_recover");
        let recovered = std::fs::read_to_string(&recovered_path).expect("读 recovered");

        let outcome = XmlDocument::parse_lenient(&pre, &limits)
            .unwrap_or_else(|e| panic!("{id}: 宽松解析失败: {e}"));
        let expected = XmlDocument::parse_strict(&recovered, &limits)
            .unwrap_or_else(|e| panic!("{id}: recovered 严格解析失败: {e}"));

        if let Err(diff) = trees_equal(&outcome.doc, &expected) {
            failures.push(format!("{id}: {diff}"));
        }
        checked += 1;
    }

    assert_eq!(
        checked, 64,
        "golden fixture 对数应为 64（48 P2/P3 + 16 P4）"
    );
    assert!(
        failures.is_empty(),
        "以下 fixture 树结构不一致:\n{}",
        failures.join("\n")
    );
}
