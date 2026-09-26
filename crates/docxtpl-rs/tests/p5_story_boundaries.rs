//! P5 DEV-0007 story 枚举边界的最小包级回归。

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use docxtpl_rs::{DocxTemplate, InlineImage, RenderContext, RenderOptions};
use serde_json::json;
use zip::write::{SimpleFileOptions, ZipWriter};
use zip::{CompressionMethod, ZipArchive};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn rewrite_rels(src: &Path, dst: &Path, relationship: &str) {
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
                    &format!("{relationship}</Relationships>"),
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
    writer.finish().expect("完成变体 zip");
}

fn add_orphan_story_and_endnotes(src: &Path, dst: &Path) {
    let mut archive = ZipArchive::new(File::open(src).expect("打开源 docx")).expect("读取 zip");
    let mut writer = ZipWriter::new(File::create(dst).expect("创建变体 docx"));
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("读取条目");
        let name = entry.name().to_string();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).expect("读取条目字节");
        if name == "[Content_Types].xml" {
            bytes = String::from_utf8(bytes)
                .expect("CT UTF-8")
                .replacen(
                    "</Types>",
                    r#"<Override PartName="/word/header99.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.header+xml"/><Override PartName="/word/endnotes.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.endnotes+xml"/></Types>"#,
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
    let broken = br#"<?xml version="1.0" encoding="UTF-8"?><w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t>{% if broken %}</w:t></w:r></w:p></w:hdr>"#;
    writer
        .start_file("word/header99.xml", SimpleFileOptions::default())
        .expect("写孤立 header");
    writer.write_all(broken).expect("写孤立 header 字节");
    writer
        .start_file("word/endnotes.xml", SimpleFileOptions::default())
        .expect("写 endnotes");
    writer.write_all(broken).expect("写 endnotes 字节");
    writer.finish().expect("完成变体 zip");
}

fn rewrite_part(src: &Path, dst: &Path, part: &str, transform: impl FnOnce(Vec<u8>) -> Vec<u8>) {
    let mut archive = ZipArchive::new(File::open(src).expect("打开源 docx")).expect("读取 zip");
    let mut writer = ZipWriter::new(File::create(dst).expect("创建变体 docx"));
    let mut transform = Some(transform);
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("读取条目");
        let name = entry.name().to_string();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).expect("读取条目字节");
        if name == part {
            bytes = transform.take().expect("目标 part 仅出现一次")(bytes);
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
    assert!(transform.is_none(), "目标 part 不存在: {part}");
    writer.finish().expect("完成变体 zip");
}

fn render_variant(relationship: &str) {
    let dir = tempfile::Builder::new()
        .prefix("p5-story-")
        .tempdir_in(root().join("target"))
        .expect("在项目 target 创建临时目录");
    let src = root().join("tests/fixtures/templates/p5_hf_basic.docx");
    let variant = dir.path().join("template.docx");
    rewrite_rels(&src, &variant, relationship);
    DocxTemplate::open(&variant)
        .expect("打开 story 变体")
        .render(&json!({}), &RenderOptions::compat())
        .expect("边界关系不得导致重复渲染或访问外部 story");
}

#[test]
fn duplicate_header_target_is_rendered_once() {
    render_variant(
        r#"<Relationship Id="rIdDuplicateHeader" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="header1.xml"/>"#,
    );
}

#[test]
fn external_header_relationship_is_ignored() {
    render_variant(
        r#"<Relationship Id="rIdExternalHeader" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/header" Target="https://example.com/header.xml" TargetMode="External"/>"#,
    );
}

#[test]
fn orphan_story_and_endnotes_are_not_rendered() {
    let dir = tempfile::Builder::new()
        .prefix("p5-orphan-")
        .tempdir_in(root().join("target"))
        .expect("在项目 target 创建临时目录");
    let src = root().join("tests/fixtures/templates/p5_hf_basic.docx");
    let variant = dir.path().join("template.docx");
    add_orphan_story_and_endnotes(&src, &variant);
    DocxTemplate::open(&variant)
        .expect("打开孤立 story/endnotes 变体")
        .render(&json!({}), &RenderOptions::compat())
        .expect("孤立 story 与 endnotes 不应进入渲染管线");
}

#[test]
fn header_image_error_reports_part_name() {
    let template = root().join("tests/fixtures/templates/p5_hf_image.docx");
    let bad = root().join("tests/fixtures/media/p4_bad.png");
    let mut context = RenderContext::new();
    context.insert(
        "himg",
        InlineImage::from_path(bad.to_str().expect("UTF-8 路径"), None, None, None)
            .expect("读取坏图片字节"),
    );
    let result = DocxTemplate::open(template)
        .expect("打开模板")
        .render_ctx(&context, &RenderOptions::compat());
    let error = match result {
        Ok(_) => panic!("页眉坏图片必须失败"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("word/header1.xml"), "{error}");
}

#[test]
fn core_properties_syntax_error_reports_part_name() {
    let dir = tempfile::Builder::new()
        .prefix("p5-core-props-")
        .tempdir_in(root().join("target"))
        .expect("在项目 target 创建临时目录");
    let src = root().join("tests/fixtures/templates/p5_hf_basic.docx");
    let variant = dir.path().join("template.docx");
    rewrite_part(&src, &variant, "docProps/core.xml", |bytes| {
        String::from_utf8(bytes)
            .expect("core UTF-8")
            .replacen("python-docx", "{% if broken %}", 1)
            .into_bytes()
    });
    let result = DocxTemplate::open(variant)
        .expect("打开模板")
        .render(&json!({}), &RenderOptions::compat());
    let error = match result {
        Ok(_) => panic!("核心属性语法错误必须失败"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("docProps/core.xml"), "{error}");
}
