//! Real Word part round-trip: read `word/document.xml` from three template
//! docx files, strictly parse → serialize → strictly parse again, requiring
//! equal tree structure and idempotent serialization; the serialized result
//! must also parse leniently without producing any diagnostics.

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
    let file = File::open(&path).unwrap_or_else(|e| panic!("opening {}: {e}", path.display()));
    let mut zip = zip::ZipArchive::new(file).expect("reading zip/docx");
    let mut entry = zip
        .by_name("word/document.xml")
        .expect("docx missing word/document.xml");
    let mut buf = Vec::new();
    entry
        .read_to_end(&mut buf)
        .expect("reading word/document.xml");
    String::from_utf8(buf).expect("document.xml is not UTF-8")
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
            .unwrap_or_else(|e| panic!("{name}: initial strict parsing failed: {e}"));
        let serialized = doc.serialize();

        let doc2 = XmlDocument::parse_strict(&serialized, &limits)
            .unwrap_or_else(|e| panic!("{name}: serialized result failed strict parsing: {e}"));
        trees_equal(&doc, &doc2).unwrap_or_else(|d| panic!("{name}: round-trip trees differ: {d}"));

        let serialized2 = doc2.serialize();
        assert_eq!(
            serialized, serialized2,
            "{name}: serialization is not idempotent"
        );

        let outcome = XmlDocument::parse_lenient(&serialized, &limits)
            .unwrap_or_else(|e| panic!("{name}: lenient parsing of serialized result failed: {e}"));
        assert!(
            outcome.diagnostics.is_empty(),
            "{name}: clean serialization should produce no diagnostics: {:?}",
            outcome.diagnostics
        );
        trees_equal(&doc, &outcome.doc)
            .unwrap_or_else(|d| panic!("{name}: lenient re-parse tree differs: {d}"));
    }
}
