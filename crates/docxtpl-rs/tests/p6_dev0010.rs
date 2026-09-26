//! DEV-0010 dynamic DOCX integration regression for the numbering-merge compatibility path.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use docxtpl_rs::{DocxTemplate, RenderOptions, RenderValue};
use zip::write::{SimpleFileOptions, ZipWriter};
use zip::{CompressionMethod, ZipArchive};

const RT_NUMBERING: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn rewrite_docx(
    src: &Path,
    dst: &Path,
    mut transform: impl FnMut(&str, Vec<u8>) -> Option<Vec<u8>>,
) {
    let mut archive =
        ZipArchive::new(File::open(src).expect("open the source docx")).expect("read the zip");
    let mut writer = ZipWriter::new(File::create(dst).expect("create the variant docx"));
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("read entry");
        let name = entry.name().to_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).expect("read entry bytes");
        let Some(bytes) = transform(&name, bytes) else {
            continue;
        };
        let method = if entry.is_dir() {
            CompressionMethod::Stored
        } else {
            entry.compression()
        };
        writer
            .start_file(
                name,
                SimpleFileOptions::default().compression_method(method),
            )
            .expect("write entry");
        writer.write_all(&bytes).expect("write entry bytes");
    }
    writer.finish().expect("finish the variant zip");
}

fn inject_body_child(src: &Path, dst: &Path, fragment: &str) {
    rewrite_docx(src, dst, |name, bytes| {
        if name != "word/document.xml" {
            return Some(bytes);
        }
        let xml = String::from_utf8(bytes).expect("document.xml UTF-8");
        assert!(xml.contains("</w:body>"), "document.xml is missing w:body");
        Some(
            xml.replacen("</w:body>", &format!("{fragment}</w:body>"), 1)
                .into_bytes(),
        )
    });
}

fn strip_empty_element_containing(xml: String, needle: &str) -> String {
    let needle_at = xml
        .find(needle)
        .expect("marker of the element to remove exists");
    let start = xml[..needle_at]
        .rfind('<')
        .expect("element to remove has a start tag");
    let end = needle_at
        + xml[needle_at..]
            .find("/>")
            .expect("element to remove is an empty element")
        + 2;
    let mut result = xml;
    result.replace_range(start..end, "");
    result
}

fn remove_main_numbering(src: &Path, dst: &Path) {
    rewrite_docx(src, dst, |name, bytes| match name {
        "word/numbering.xml" => None,
        "word/_rels/document.xml.rels" => Some(
            strip_empty_element_containing(
                String::from_utf8(bytes).expect("document rels UTF-8"),
                RT_NUMBERING,
            )
            .into_bytes(),
        ),
        "[Content_Types].xml" => Some(
            strip_empty_element_containing(
                String::from_utf8(bytes).expect("Content Types UTF-8"),
                "PartName=\"/word/numbering.xml\"",
            )
            .into_bytes(),
        ),
        _ => Some(bytes),
    });
}

#[test]
fn accepts_numbered_subdoc_when_main_has_no_numbering_part() {
    let dir = tempfile::Builder::new()
        .prefix("p6-dev0010-missing-numbering-")
        .tempdir_in(root().join("target"))
        .expect("create a tempdir in the project target");
    let main = dir.path().join("main-without-numbering.docx");
    let sub = dir.path().join("numbered-sub.docx");
    remove_main_numbering(
        &root().join("tests/fixtures/templates/p6_subdoc_basic.docx"),
        &main,
    );
    inject_body_child(
        &root().join("tests/fixtures/templates/p6_subdoc_basic_sub.docx"),
        &sub,
        r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>numbered</w:t></w:r></w:p>"#,
    );

    let template =
        DocxTemplate::open(&main).expect("open the main template without a numbering part");
    let mut session = template
        .render_session(&RenderOptions::compat())
        .expect("create the render session");
    let value = session.new_subdoc(&sub).expect(
        "should lazily create and merge numbering when the main package lacks a numbering part",
    );
    assert!(matches!(value, RenderValue::Subdoc(_)));
}

#[test]
fn restarts_first_numbering_modification_block() {
    let dir = tempfile::Builder::new()
        .prefix("p6-dev0010-restart-numbering-")
        .tempdir_in(root().join("target"))
        .expect("create a tempdir in the project target");
    let sub = dir.path().join("list-number-sub.docx");
    inject_body_child(
        &root().join("tests/fixtures/templates/p6_subdoc_basic_sub.docx"),
        &sub,
        r#"<w:p><w:pPr><w:pStyle w:val="ListNumber"/></w:pPr><w:r><w:t>restart</w:t></w:r></w:p>"#,
    );

    let template = DocxTemplate::open(root().join("tests/fixtures/templates/p6_subdoc_basic.docx"))
        .expect("open the main template");
    let mut session = template
        .render_session(&RenderOptions::compat())
        .expect("create the render session");
    let value = session
        .new_subdoc(&sub)
        .expect("restart_first_numbering should copy numbering and rewrite paragraph numPr");
    let RenderValue::Subdoc(fragment) = value else {
        panic!("new_subdoc should return a Subdoc value");
    };
    assert!(fragment.as_str().contains("<w:numId w:val=\"10\"/>"));
}
