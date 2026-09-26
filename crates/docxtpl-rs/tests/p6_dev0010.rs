//! DEV-0010 编号合并拒绝路径的动态 DOCX 集成回归。

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use docxtpl_opc::OpcError;
use docxtpl_rs::{DocxTemplate, Error, RenderOptions};
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
    let mut archive = ZipArchive::new(File::open(src).expect("打开源 docx")).expect("读取 zip");
    let mut writer = ZipWriter::new(File::create(dst).expect("创建变体 docx"));
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("读取条目");
        let name = entry.name().to_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).expect("读取条目字节");
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
            .expect("写条目");
        writer.write_all(&bytes).expect("写条目字节");
    }
    writer.finish().expect("完成变体 zip");
}

fn inject_body_child(src: &Path, dst: &Path, fragment: &str) {
    rewrite_docx(src, dst, |name, bytes| {
        if name != "word/document.xml" {
            return Some(bytes);
        }
        let xml = String::from_utf8(bytes).expect("document.xml UTF-8");
        assert!(xml.contains("</w:body>"), "document.xml 缺少 w:body");
        Some(
            xml.replacen("</w:body>", &format!("{fragment}</w:body>"), 1)
                .into_bytes(),
        )
    });
}

fn strip_empty_element_containing(xml: String, needle: &str) -> String {
    let needle_at = xml.find(needle).expect("待删除元素的标识存在");
    let start = xml[..needle_at].rfind('<').expect("待删除元素有开始标签");
    let end = needle_at + xml[needle_at..].find("/>").expect("待删除元素是空元素") + 2;
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

fn malformed_reason(err: Error) -> String {
    match err {
        Error::Opc(OpcError::Malformed { reason }) => reason,
        other => panic!("应返回公开的 OPC Malformed 拒绝错误，实际为: {other:?}"),
    }
}

#[test]
fn rejects_numbered_subdoc_when_main_has_no_numbering_part() {
    let dir = tempfile::Builder::new()
        .prefix("p6-dev0010-missing-numbering-")
        .tempdir_in(root().join("target"))
        .expect("在项目 target 创建临时目录");
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

    let template = DocxTemplate::open(&main).expect("打开缺少 numbering part 的主模板");
    let mut session = template
        .render_session(&RenderOptions::compat())
        .expect("创建渲染会话");
    let reason = malformed_reason(
        session
            .new_subdoc(&sub)
            .expect_err("主包缺少 numbering part 时必须拒绝编号子文档"),
    );
    assert!(reason.contains("word/numbering.xml"), "实际原因: {reason}");
    assert!(reason.contains("无法合并子文档编号"), "实际原因: {reason}");
}

#[test]
fn rejects_restart_first_numbering_modification_block() {
    let dir = tempfile::Builder::new()
        .prefix("p6-dev0010-restart-numbering-")
        .tempdir_in(root().join("target"))
        .expect("在项目 target 创建临时目录");
    let sub = dir.path().join("list-number-sub.docx");
    inject_body_child(
        &root().join("tests/fixtures/templates/p6_subdoc_basic_sub.docx"),
        &sub,
        r#"<w:p><w:pPr><w:pStyle w:val="ListNumber"/></w:pPr><w:r><w:t>restart</w:t></w:r></w:p>"#,
    );

    let template = DocxTemplate::open(root().join("tests/fixtures/templates/p6_subdoc_basic.docx"))
        .expect("打开主模板");
    let mut session = template
        .render_session(&RenderOptions::compat())
        .expect("创建渲染会话");
    let reason = malformed_reason(
        session
            .new_subdoc(&sub)
            .expect_err("restart_first_numbering 修改块必须拒绝"),
    );
    assert!(
        reason.contains("restart_first_numbering"),
        "实际原因: {reason}"
    );
    assert!(reason.contains("word/numbering.xml"), "实际原因: {reason}");
    assert!(reason.contains("ListNumber"), "实际原因: {reason}");
    assert!(reason.contains("主包不支持"), "实际原因: {reason}");
}
