//! P7 替换族与图片映射的非 oracle 分支回归。

use std::fs::File;
use std::io::{Cursor, Read, Write};
use std::path::Path;
use std::path::PathBuf;

use docxtpl_opc::{Package, PackageLimits};
use docxtpl_rs::{DocxTemplate, RenderContext, RenderOptions};
use zip::write::{SimpleFileOptions, ZipWriter};
use zip::{CompressionMethod, ZipArchive};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn template(name: &str) -> DocxTemplate {
    DocxTemplate::open(
        root()
            .join("tests/fixtures/templates")
            .join(format!("{name}.docx")),
    )
    .unwrap_or_else(|e| panic!("打开 {name}: {e}"))
}

fn media(name: &str) -> Vec<u8> {
    std::fs::read(root().join("tests/fixtures/media").join(name))
        .unwrap_or_else(|e| panic!("读取 {name}: {e}"))
}

fn rewrite_document(src: &Path, dst: &Path, from: &str, to: &str) {
    let mut archive = ZipArchive::new(File::open(src).expect("打开源 docx")).expect("读取 zip");
    let mut writer = ZipWriter::new(File::create(dst).expect("创建变体 docx"));
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("读取条目");
        let name = entry.name().to_string();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).expect("读取条目字节");
        if name == "word/document.xml" {
            bytes = String::from_utf8(bytes)
                .expect("document.xml UTF-8")
                .replacen(from, to, 1)
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

#[test]
fn picture_map_reports_relative_target() {
    let map = template("p7_media_body")
        .picture_map()
        .expect("读取图片映射");
    assert_eq!(
        map.get("image.png").map(String::as_str),
        Some("media/image1.png")
    );
}

#[test]
fn reset_replacements_clears_missing_picture_and_media_registration() {
    let tpl = template("p7_pic_missing");
    let mut session = tpl
        .render_session(&RenderOptions::compat())
        .expect("创建会话");
    session
        .replace_pic("nope.png", media("p7_media3x2.png"))
        .replace_media(media("p7_dummy.png"), media("p7_new4x4.png"))
        .reset_replacements();
    session
        .finish(&RenderContext::new())
        .expect("reset 后不得残留替换或 missing-picture 错误");
}

#[test]
fn zipname_has_priority_over_embedded_crc() {
    let tpl = template("p7_embedded_zipname");
    let zip_bytes = b"zipname-wins";
    let mut session = tpl
        .render_session(&RenderOptions::compat())
        .expect("创建会话");
    session
        .replace_embedded(media("p7_embed_orig1.bin"), b"crc-loses")
        .replace_zipname("word/embeddings/p7_ole1.bin", zip_bytes);
    let rendered = session
        .finish(&RenderContext::new())
        .expect("完成替换")
        .to_bytes()
        .expect("写出包");
    let pkg =
        Package::from_reader(Cursor::new(rendered), &PackageLimits::default()).expect("重开输出包");
    assert_eq!(
        pkg.part("word/embeddings/p7_ole1.bin")
            .expect("嵌入 part")
            .bytes(),
        zip_bytes
    );
}

#[test]
fn unmatched_byte_replacements_are_silent() {
    let tpl = template("p7_media_body");
    let mut session = tpl
        .render_session(&RenderOptions::compat())
        .expect("创建会话");
    session
        .replace_media(b"missing-media", b"new")
        .replace_embedded(b"missing-embedded", b"new")
        .replace_zipname("word/missing.bin", b"new");
    session
        .finish(&RenderContext::new())
        .expect("三类字节替换未命中必须静默");
}

#[test]
fn replace_pic_matches_description() {
    let workspace_target = root().join("target");
    let dir = tempfile::Builder::new()
        .prefix("p7-descr-")
        .tempdir_in(workspace_target)
        .expect("在项目 target 创建临时目录");
    let src = root().join("tests/fixtures/templates/p7_pic_match.docx");
    let variant = dir.path().join("template.docx");
    rewrite_document(
        &src,
        &variant,
        r#"title="my-title""#,
        r#"descr="my-description""#,
    );
    let tpl = DocxTemplate::open(&variant).expect("打开 descr 变体");
    let mut session = tpl
        .render_session(&RenderOptions::compat())
        .expect("创建会话");
    session.replace_pic("my-description", media("p7_new4x4.png"));
    session
        .finish(&RenderContext::new())
        .expect("descr 应命中图片");
}

#[test]
fn first_registered_pic_key_shadows_later_key_on_same_picture() {
    let tpl = template("p7_pic_match");
    let mut session = tpl
        .render_session(&RenderOptions::compat())
        .expect("创建会话");
    session
        .replace_pic("titled-2x2.png", media("p7_media3x2.png"))
        .replace_pic("my-title", media("p7_new4x4.png"));
    let err = match session.finish(&RenderContext::new()) {
        Ok(_) => panic!("后注册 title 被同图 name 遮蔽，应保持未命中"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("Picture my-title not found"));
}

#[test]
fn duplicate_anchor_url_reuses_relationship_id() {
    let template = root().join("tests/fixtures/templates/p4_rt_url.docx");
    let tpl = DocxTemplate::open(template).expect("打开模板");
    let mut session = tpl
        .render_session(&RenderOptions::compat())
        .expect("创建渲染会话");
    let first = session.build_url_id("https://example.com/a?x=1&y=2");
    let second = session.build_url_id("https://example.com/a?x=1&y=2");
    assert_eq!(first, second);
}
