//! Golden 差分：`patch_xml(word/document.xml 原文)` 必须与上游 docxtpl 0.20.2
//! `DocxTemplate.patch_xml` 的产物（`tests/fixtures/stages/<id>.full_patched.xml`）
//! 逐字节相等。golden 由 `tests/fixtures/dump_stages.py` 生成，不得修改或放宽比较。

use docxtpl_compat::patch_xml;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

const STAGES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/stages");
const TEMPLATES_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/templates"
);

const GOLDEN_SUFFIX: &str = ".full_patched.xml";
// P0–P3 49 + P4 18 + P5 7 + P6 5 + P7 7 + P7b 16 = 102。
const EXPECTED_GOLDEN_COUNT: usize = 102;

/// 枚举 stages 目录下全部 `<id>.full_patched.xml`，按 id 排序。
fn list_golden_ids() -> Vec<String> {
    let mut ids: Vec<String> = fs::read_dir(STAGES_DIR)
        .expect("stages directory exists")
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.strip_suffix(GOLDEN_SUFFIX).map(str::to_owned)
        })
        .collect();
    ids.sort();
    ids
}

/// 读取模板 docx 内的 `word/document.xml` UTF-8 原文。
fn read_document_xml(id: &str) -> Vec<u8> {
    let path = Path::new(TEMPLATES_DIR).join(format!("{id}.docx"));
    let file = fs::File::open(&path).unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
    let mut archive = zip::ZipArchive::new(file).expect("open docx zip");
    let mut entry = archive
        .by_name("word/document.xml")
        .expect("docx contains word/document.xml");
    let mut bytes = Vec::with_capacity(usize::try_from(entry.size()).unwrap_or(0));
    entry.read_to_end(&mut bytes).expect("read document.xml");
    bytes
}

fn golden_path(id: &str) -> PathBuf {
    Path::new(STAGES_DIR).join(format!("{id}{GOLDEN_SUFFIX}"))
}

/// 以字节偏移为中心向两侧各截取最多 80 字符，并对齐到 UTF-8 字符边界。
fn context_around(text: &str, offset: usize) -> String {
    let mut center = offset.min(text.len());
    while center < text.len() && !text.is_char_boundary(center) {
        center += 1;
    }

    let mut start = center;
    for _ in 0..80 {
        match text[..start].chars().next_back() {
            Some(ch) => start -= ch.len_utf8(),
            None => break,
        }
    }
    let mut end = center;
    for _ in 0..80 {
        match text[end..].chars().next() {
            Some(ch) => end += ch.len_utf8(),
            None => break,
        }
    }
    text[start..end].replace('\r', "\\r").replace('\n', "\\n")
}

#[test]
fn golden_set_is_complete() {
    let ids = list_golden_ids();
    assert_eq!(
        ids.len(),
        EXPECTED_GOLDEN_COUNT,
        "full_patched golden 数量应为 {EXPECTED_GOLDEN_COUNT}，实际 {}（缺失或新增需同步本测试）",
        ids.len()
    );
}

#[test]
fn patch_xml_matches_every_golden_byte_for_byte() {
    let ids = list_golden_ids();
    assert_eq!(ids.len(), EXPECTED_GOLDEN_COUNT);

    let mut failures: Vec<String> = Vec::new();
    for id in &ids {
        let raw_bytes = read_document_xml(id);
        let raw = String::from_utf8(raw_bytes).expect("document.xml is UTF-8");
        let patched = patch_xml(&raw);
        let golden_bytes = fs::read(golden_path(id)).expect("read golden file");

        if patched.as_bytes() != golden_bytes.as_slice() {
            let golden = String::from_utf8_lossy(&golden_bytes);
            let first_diff = patched
                .as_bytes()
                .iter()
                .zip(golden_bytes.iter())
                .position(|(a, b)| a != b)
                .unwrap_or_else(|| patched.len().min(golden_bytes.len()));
            failures.push(format!(
                "id={id} 首个差异字节偏移={first_diff}\n  got({}B): {}\n  exp({}B): {}",
                patched.len(),
                context_around(&patched, first_diff),
                golden_bytes.len(),
                context_around(&golden, first_diff),
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} 个 golden 未逐字节对齐（共 {} 个）:\n{}",
        failures.len(),
        ids.len(),
        failures.join("\n")
    );
}

#[test]
fn patching_golden_outputs_again_does_not_panic() {
    // 不要求幂等，只要求对任意 golden 输出再次调用 patch_xml 不 panic。
    for id in list_golden_ids() {
        let golden = fs::read_to_string(golden_path(&id)).expect("read golden file");
        let _ = patch_xml(&golden);
    }
}
