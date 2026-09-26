//! P5 核心属性 part 缺失与编码错误的包级回归。

use std::fs::File;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use docxtpl_rs::{DocxTemplate, Error, RenderOptions};
use serde_json::json;
use zip::write::{SimpleFileOptions, ZipWriter};
use zip::{CompressionMethod, ZipArchive};

const CORE_PART: &str = "docProps/core.xml";
const CORE_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties";
const CORE_CONTENT_TYPE: &str = "application/vnd.openxmlformats-package.core-properties+xml";

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
        let name = entry.name().to_string();
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

fn zip_part(bytes: &[u8], name: &str) -> Vec<u8> {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).expect("读取输出 zip");
    let mut part = archive.by_name(name).expect("输出 part 存在");
    let mut contents = Vec::new();
    part.read_to_end(&mut contents).expect("读取输出 part");
    contents
}

#[test]
fn missing_core_properties_part_is_recreated_like_python_docx() {
    let dir = tempfile::Builder::new()
        .prefix("p5-core-missing-")
        .tempdir_in(root().join("target"))
        .expect("在项目 target 创建临时目录");
    let src = root().join("tests/fixtures/templates/p5_hf_basic.docx");
    let variant = dir.path().join("template.docx");
    rewrite_docx(&src, &variant, |name, bytes| {
        if name == CORE_PART {
            return None;
        }
        let xml = match name {
            "_rels/.rels" => String::from_utf8(bytes).expect("根 rels UTF-8").replace(
                &format!(
                    r#"<Relationship Id="rId3" Type="{CORE_REL_TYPE}" Target="docProps/core.xml"/>"#
                ),
                "",
            ),
            "[Content_Types].xml" => String::from_utf8(bytes).expect("CT UTF-8").replace(
                &format!(
                    r#"<Override PartName="/docProps/core.xml" ContentType="{CORE_CONTENT_TYPE}"/>"#
                ),
                "",
            ),
            _ => return Some(bytes),
        };
        Some(xml.into_bytes())
    });

    let output = DocxTemplate::open(&variant)
        .expect("无核心属性的合法模板应能打开")
        .render(&json!({}), &RenderOptions::compat())
        .expect("上游会创建默认核心属性 part")
        .to_bytes()
        .expect("序列化输出");

    let core = String::from_utf8(zip_part(&output, CORE_PART)).expect("核心属性 UTF-8");
    assert!(
        core.contains("<dc:title>Word Document</dc:title>"),
        "{core}"
    );
    assert!(
        core.contains(
            "<cp:lastModifiedBy xmlns:cp=\"http://schemas.openxmlformats.org/officeDocument/2006/custom-properties\">python-docx</cp:lastModifiedBy>"
        ),
        "docxtpl 0.20.2 + docxcompose 2.2.0 会重绑定该子节点的 cp 前缀: {core}"
    );

    let rels = String::from_utf8(zip_part(&output, "_rels/.rels")).expect("根 rels UTF-8");
    assert!(rels.contains(CORE_REL_TYPE), "{rels}");
    assert!(rels.contains("Target=\"docProps/core.xml\""), "{rels}");

    let content_types =
        String::from_utf8(zip_part(&output, "[Content_Types].xml")).expect("CT UTF-8");
    assert!(content_types.contains(CORE_CONTENT_TYPE), "{content_types}");
}

#[test]
fn non_utf8_core_properties_reports_part_name() {
    let dir = tempfile::Builder::new()
        .prefix("p5-core-encoding-")
        .tempdir_in(root().join("target"))
        .expect("在项目 target 创建临时目录");
    let src = root().join("tests/fixtures/templates/p5_hf_basic.docx");
    let variant = dir.path().join("template.docx");
    rewrite_docx(&src, &variant, |name, mut bytes| {
        if name == CORE_PART {
            bytes.push(0xff);
        }
        Some(bytes)
    });

    let result = DocxTemplate::open(&variant)
        .expect("OPC 层不预解码 XML part")
        .render(&json!({}), &RenderOptions::compat());
    let error = match result {
        Ok(_) => panic!("非 UTF-8 核心属性必须失败"),
        Err(error) => error,
    };
    assert!(matches!(
        &error,
        Error::NotUtf8 { part, .. } if part == CORE_PART
    ));
    assert!(error.to_string().contains(CORE_PART), "{error}");
}
