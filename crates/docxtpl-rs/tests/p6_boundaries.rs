//! P6 已公开拒绝边界的最小 DOCX 集成回归。

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use docxtpl_rs::{DocxTemplate, RenderOptions};
use zip::write::{SimpleFileOptions, ZipWriter};
use zip::{CompressionMethod, ZipArchive};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn rewrite_document(src: &Path, dst: &Path, fragment: &str) {
    let mut archive = ZipArchive::new(File::open(src).expect("打开源 docx")).expect("读取 zip");
    let out = File::create(dst).expect("创建变体 docx");
    let mut writer = ZipWriter::new(out);
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("读取条目");
        let name = entry.name().to_string();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).expect("读取条目字节");
        if name == "word/document.xml" {
            let xml = String::from_utf8(bytes).expect("document.xml UTF-8");
            bytes = xml
                .replacen("</w:body>", &format!("{fragment}</w:body>"), 1)
                .into_bytes();
        }
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
            .expect("写条目");
        writer.write_all(&bytes).expect("写条目字节");
    }
    writer.finish().expect("完成变体 zip");
}

fn add_custom_properties(src: &Path, dst: &Path) {
    let mut archive = ZipArchive::new(File::open(src).expect("打开源 docx")).expect("读取 zip");
    let mut writer = ZipWriter::new(File::create(dst).expect("创建变体 docx"));
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("读取条目");
        let name = entry.name().to_string();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).expect("读取条目字节");
        if name == "word/_rels/document.xml.rels" {
            bytes = String::from_utf8(bytes)
                .expect("rels UTF-8")
                .replacen(
                    "</Relationships>",
                    r#"<Relationship Id="rIdCustomProps" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/custom-properties" Target="../docProps/custom.xml"/></Relationships>"#,
                    1,
                )
                .into_bytes();
        } else if name == "[Content_Types].xml" {
            bytes = String::from_utf8(bytes)
                .expect("CT UTF-8")
                .replacen(
                    "</Types>",
                    r#"<Override PartName="/docProps/custom.xml" ContentType="application/vnd.openxmlformats-officedocument.custom-properties+xml"/></Types>"#,
                    1,
                )
                .into_bytes();
        }
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
            .expect("写条目");
        writer.write_all(&bytes).expect("写条目字节");
    }
    writer
        .start_file("docProps/custom.xml", SimpleFileOptions::default())
        .expect("写 custom properties part");
    writer
        .write_all(br#"<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/custom-properties"/>"#)
        .expect("写 custom properties 字节");
    writer.finish().expect("完成变体 zip");
}

fn assert_subdoc_rejected(fragment: &str, expected: &str) {
    let workspace_target = root().join("target");
    let dir = tempfile::Builder::new()
        .prefix("p6-boundary-")
        .tempdir_in(workspace_target)
        .expect("在项目 target 创建临时目录");
    let base = root().join("tests/fixtures/templates/p6_subdoc_basic_sub.docx");
    let variant = dir.path().join("sub.docx");
    rewrite_document(&base, &variant, fragment);

    let main = DocxTemplate::open(root().join("tests/fixtures/templates/p6_subdoc_basic.docx"))
        .expect("打开主模板");
    let mut session = main
        .render_session(&RenderOptions::compat())
        .expect("创建会话");
    let err = session.new_subdoc(&variant).expect_err("边界必须被拒绝");
    assert!(err.to_string().contains(expected), "实际错误: {err}");
}

#[test]
fn rejects_smartart_reference() {
    assert_subdoc_rejected(
        r#"<w:p xmlns:dgm="http://schemas.openxmlformats.org/officeDocument/2006/diagram" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><dgm:relIds r:dm="rId999"/></w:p>"#,
        "SmartArt",
    );
}

#[test]
fn rejects_vml_shape_image() {
    assert_subdoc_rejected(
        r#"<w:p xmlns:v="urn:schemas-microsoft-com:vml"><v:shape><v:imagedata/></v:shape></w:p>"#,
        "VML",
    );
}

#[test]
fn rejects_footnote_reference() {
    assert_subdoc_rejected(
        r#"<w:p><w:r><w:footnoteReference w:id="1"/></w:r></w:p>"#,
        "脚注引用",
    );
}

#[test]
fn rejects_when_both_documents_have_multiple_sections() {
    let workspace_target = root().join("target");
    let dir = tempfile::Builder::new()
        .prefix("p6-sections-")
        .tempdir_in(workspace_target)
        .expect("在项目 target 创建临时目录");
    let main_variant = dir.path().join("main.docx");
    let sub_variant = dir.path().join("sub.docx");
    rewrite_document(
        &root().join("tests/fixtures/templates/p6_subdoc_basic.docx"),
        &main_variant,
        "<w:sectPr/>",
    );
    rewrite_document(
        &root().join("tests/fixtures/templates/p6_subdoc_basic_sub.docx"),
        &sub_variant,
        "<w:sectPr/>",
    );
    let main = DocxTemplate::open(&main_variant).expect("打开多分节主模板");
    let mut session = main
        .render_session(&RenderOptions::compat())
        .expect("创建会话");
    let err = session
        .new_subdoc(&sub_variant)
        .expect_err("两侧多分节必须拒绝");
    assert!(
        err.to_string().contains("fix_section_types"),
        "实际错误: {err}"
    );
}

#[test]
fn rejects_custom_properties_part() {
    let dir = tempfile::Builder::new()
        .prefix("p6-custom-props-")
        .tempdir_in(root().join("target"))
        .expect("在项目 target 创建临时目录");
    let variant = dir.path().join("sub.docx");
    add_custom_properties(
        &root().join("tests/fixtures/templates/p6_subdoc_basic_sub.docx"),
        &variant,
    );
    let main = DocxTemplate::open(root().join("tests/fixtures/templates/p6_subdoc_basic.docx"))
        .expect("打开主模板");
    let mut session = main
        .render_session(&RenderOptions::compat())
        .expect("创建会话");
    let err = session
        .new_subdoc(&variant)
        .expect_err("custom properties 必须拒绝");
    assert!(err.to_string().contains("custom.xml"), "实际错误: {err}");
}

#[test]
fn rejects_nsid_randomization_path() {
    assert_subdoc_rejected(
        r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>numbered</w:t></w:r></w:p>"#,
        "w:nsid",
    );
}
