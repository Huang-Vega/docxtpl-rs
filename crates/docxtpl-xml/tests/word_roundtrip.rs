//! 真实 Word part 往返：读取 3 个模板 docx 的 `word/document.xml`，
//! 严格解析 → 序列化 → 再严格解析，要求树结构相等、序列化幂等；
//! 序列化结果再用宽松解析不产生任何诊断。

mod common;

use std::fs::File;
use std::io::Read;

use common::trees_equal;
use docxtpl_xml::{XmlDocument, XmlLimits};

fn templates_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("templates")
}

fn read_document_xml(name: &str) -> String {
    let path = templates_dir().join(name);
    let file = File::open(&path).unwrap_or_else(|e| panic!("打开 {}: {e}", path.display()));
    let mut zip = zip::ZipArchive::new(file).expect("读取 zip/docx");
    let mut entry = zip
        .by_name("word/document.xml")
        .expect("docx 缺少 word/document.xml");
    let mut buf = Vec::new();
    entry.read_to_end(&mut buf).expect("读取 word/document.xml");
    String::from_utf8(buf).expect("document.xml 不是 UTF-8")
}

#[test]
fn roundtrip_three_real_docx() {
    let limits = XmlLimits::default();
    for name in [
        "rt_minimal.docx",
        "rt_paragraphs.docx",
        "rt_table_basic.docx",
    ] {
        let xml = read_document_xml(name);

        let doc = XmlDocument::parse_strict(&xml, &limits)
            .unwrap_or_else(|e| panic!("{name}: 首次严格解析失败: {e}"));
        let serialized = doc.serialize();

        let doc2 = XmlDocument::parse_strict(&serialized, &limits)
            .unwrap_or_else(|e| panic!("{name}: 序列化结果无法严格解析: {e}"));
        trees_equal(&doc, &doc2).unwrap_or_else(|d| panic!("{name}: 往返树不一致: {d}"));

        let serialized2 = doc2.serialize();
        assert_eq!(serialized, serialized2, "{name}: 序列化不幂等");

        let outcome = XmlDocument::parse_lenient(&serialized, &limits)
            .unwrap_or_else(|e| panic!("{name}: 序列化结果宽松解析失败: {e}"));
        assert!(
            outcome.diagnostics.is_empty(),
            "{name}: 干净序列化不应产生诊断: {:?}",
            outcome.diagnostics
        );
        trees_equal(&doc, &outcome.doc)
            .unwrap_or_else(|d| panic!("{name}: 宽松重解析树不一致: {d}"));
    }
}
