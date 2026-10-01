//! Minimal package-level regressions for the P5 DEV-0007 story enumeration boundaries.

mod test_support;

use std::fs::File;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use docxtpl_rs::{DocxTemplate, InlineImage, RenderContext, RenderOptions};
use serde_json::json;
use zip::write::{SimpleFileOptions, ZipWriter};
use zip::{CompressionMethod, ZipArchive};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn rewrite_rels(src: &Path, dst: &Path, relationship: &str) {
    let mut archive =
        ZipArchive::new(File::open(src).expect("open the source docx")).expect("read the zip");
    let mut writer = ZipWriter::new(File::create(dst).expect("create the variant docx"));
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("read entry");
        let name = entry.name().to_string();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).expect("read entry bytes");
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
            .expect("write entry");
        writer.write_all(&bytes).expect("write entry bytes");
    }
    writer.finish().expect("finish the variant zip");
}

fn add_orphan_story_and_endnotes(src: &Path, dst: &Path) {
    let mut archive =
        ZipArchive::new(File::open(src).expect("open the source docx")).expect("read the zip");
    let mut writer = ZipWriter::new(File::create(dst).expect("create the variant docx"));
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("read entry");
        let name = entry.name().to_string();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).expect("read entry bytes");
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
            .expect("write entry");
        writer.write_all(&bytes).expect("write entry bytes");
    }
    let broken = br#"<?xml version="1.0" encoding="UTF-8"?><w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t>{% if broken %}</w:t></w:r></w:p></w:hdr>"#;
    writer
        .start_file("word/header99.xml", SimpleFileOptions::default())
        .expect("write the orphan header");
    writer
        .write_all(broken)
        .expect("write the orphan header bytes");
    writer
        .start_file("word/endnotes.xml", SimpleFileOptions::default())
        .expect("write the endnotes");
    writer
        .write_all(br#"<?xml version="1.0" encoding="UTF-8"?><w:endnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:endnote w:id="1"><w:p><w:r><w:t>END {{ value }}</w:t></w:r></w:p></w:endnote></w:endnotes>"#)
        .expect("write the endnotes bytes");
    writer.finish().expect("finish the variant zip");
}

fn rewrite_part(src: &Path, dst: &Path, part: &str, transform: impl FnOnce(Vec<u8>) -> Vec<u8>) {
    let mut archive =
        ZipArchive::new(File::open(src).expect("open the source docx")).expect("read the zip");
    let mut writer = ZipWriter::new(File::create(dst).expect("create the variant docx"));
    let mut transform = Some(transform);
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("read entry");
        let name = entry.name().to_string();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).expect("read entry bytes");
        if name == part {
            bytes = transform
                .take()
                .expect("the target part occurs exactly once")(bytes);
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
            .expect("write entry");
        writer.write_all(&bytes).expect("write entry bytes");
    }
    assert!(transform.is_none(), "target part does not exist: {part}");
    writer.finish().expect("finish the variant zip");
}

fn zip_part_from_path(path: &Path, name: &str) -> Vec<u8> {
    let mut archive =
        ZipArchive::new(File::open(path).expect("open the docx")).expect("read the zip");
    let mut entry = archive.by_name(name).expect("the target part exists");
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes).expect("read the part");
    bytes
}

fn zip_part_from_bytes(docx: &[u8], name: &str) -> Vec<u8> {
    let mut archive = ZipArchive::new(Cursor::new(docx)).expect("read the output zip");
    let mut entry = archive
        .by_name(name)
        .expect("the output target part exists");
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes).expect("read the output part");
    bytes
}

fn render_variant(relationship: &str) {
    let dir = tempfile::Builder::new()
        .prefix("p5-story-")
        .tempdir_in(test_support::target_dir())
        .expect("create a tempdir in the project target");
    let src = root().join("tests/fixtures/templates/p5_hf_basic.docx");
    let variant = dir.path().join("template.docx");
    rewrite_rels(&src, &variant, relationship);
    DocxTemplate::open(&variant)
        .expect("open the story variant")
        .render(&json!({}), &RenderOptions::compat())
        .expect("boundary relationships must not cause duplicate rendering or access to external stories");
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
fn relationship_types_that_only_end_in_header_or_footer_are_not_stories() {
    const HEADER_REL: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header";
    const FOOTER_REL: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer";
    let dir = tempfile::Builder::new()
        .prefix("p5-story-rel-type-")
        .tempdir_in(test_support::target_dir())
        .expect("create a tempdir in the project target");
    let src = root().join("tests/fixtures/templates/p5_hf_basic.docx");
    let variant = dir.path().join("template.docx");
    rewrite_part(&src, &variant, "word/_rels/document.xml.rels", |bytes| {
        String::from_utf8(bytes)
            .expect("rels UTF-8")
            .replace(HEADER_REL, "https://example.invalid/custom/header")
            .replace(FOOTER_REL, "https://example.invalid/custom/footer")
            .into_bytes()
    });

    let header_before = zip_part_from_path(&variant, "word/header1.xml");
    let footer_before = zip_part_from_path(&variant, "word/footer1.xml");
    let output = DocxTemplate::open(&variant)
        .expect("open the relationship-type variant")
        .render(
            &json!({"header_title": "SHOULD-NOT-RENDER"}),
            &RenderOptions::compat(),
        )
        .expect("custom relationships that only share a suffix must be ignored")
        .to_bytes()
        .expect("serialize the output");
    let header_after = zip_part_from_bytes(&output, "word/header1.xml");
    let footer_after = zip_part_from_bytes(&output, "word/footer1.xml");
    assert_eq!(
        header_after, header_before,
        "a non-official full header URI must not trigger story rendering"
    );
    assert_eq!(
        footer_after, footer_before,
        "a non-official full footer URI must not trigger story rendering"
    );
}

#[test]
fn orphan_story_is_ignored_but_endnotes_are_rendered() {
    let dir = tempfile::Builder::new()
        .prefix("p5-orphan-")
        .tempdir_in(test_support::target_dir())
        .expect("create a tempdir in the project target");
    let src = root().join("tests/fixtures/templates/p5_hf_basic.docx");
    let variant = dir.path().join("template.docx");
    add_orphan_story_and_endnotes(&src, &variant);
    let output = DocxTemplate::open(&variant)
        .expect("open the orphan story/endnotes variant")
        .render(&json!({"value": "rendered"}), &RenderOptions::compat())
        .expect("render the package")
        .to_bytes()
        .expect("serialize rendered package");
    let endnotes = String::from_utf8(zip_part_from_bytes(&output, "word/endnotes.xml"))
        .expect("endnotes are UTF-8");
    assert!(endnotes.contains("END rendered"), "{endnotes}");
}

#[test]
fn header_image_error_reports_part_name() {
    let template = root().join("tests/fixtures/templates/p5_hf_image.docx");
    let bad = root().join("tests/fixtures/media/p4_bad.png");
    let mut context = RenderContext::new();
    context.insert(
        "himg",
        InlineImage::from_path(bad.to_str().expect("the path is UTF-8"), None, None, None)
            .expect("read the bad image bytes"),
    );
    let result = DocxTemplate::open(template)
        .expect("open the template")
        .render_ctx(&context, &RenderOptions::compat());
    let error = match result {
        Ok(_) => panic!("a bad header image must fail"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("word/header1.xml"), "{error}");
}

#[test]
fn core_properties_syntax_error_reports_part_name() {
    let dir = tempfile::Builder::new()
        .prefix("p5-core-props-")
        .tempdir_in(test_support::target_dir())
        .expect("create a tempdir in the project target");
    let src = root().join("tests/fixtures/templates/p5_hf_basic.docx");
    let variant = dir.path().join("template.docx");
    rewrite_part(&src, &variant, "docProps/core.xml", |bytes| {
        String::from_utf8(bytes)
            .expect("core UTF-8")
            .replacen("python-docx", "{% if broken %}", 1)
            .into_bytes()
    });
    let result = DocxTemplate::open(variant)
        .expect("open the template")
        .render(&json!({}), &RenderOptions::compat());
    let error = match result {
        Ok(_) => panic!("a core-properties syntax error must fail"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("docProps/core.xml"), "{error}");
}

#[test]
fn multi_section_footnotes_are_rendered_once() {
    let dir = tempfile::Builder::new()
        .prefix("p5-footnotes-once-")
        .tempdir_in(test_support::target_dir())
        .expect("create a tempdir in the project target");
    let src = root().join("tests/fixtures/templates/p5_footnotes_basic.docx");
    let with_sections = dir.path().join("two-sections.docx");
    rewrite_part(&src, &with_sections, "word/document.xml", |bytes| {
        let mut xml = String::from_utf8(bytes).expect("document UTF-8");
        let start = xml.rfind("<w:sectPr").expect("the final sectPr");
        let relative_end = xml[start..]
            .find("</w:sectPr>")
            .expect("the sectPr closing tag")
            + "</w:sectPr>".len();
        let sect_pr = xml[start..start + relative_end].to_string();
        xml.insert_str(
            start,
            &format!("<w:p><w:pPr>{sect_pr}</w:pPr><w:r><w:t>section one</w:t></w:r></w:p>"),
        );
        xml.into_bytes()
    });
    let document = String::from_utf8(zip_part_from_path(&with_sections, "word/document.xml"))
        .expect("multi-section document UTF-8");
    assert_eq!(
        document.matches("<w:sectPr").count(),
        2,
        "regression fixture must contain two sections"
    );
    let variant = dir.path().join("template.docx");
    rewrite_part(&with_sections, &variant, "word/footnotes.xml", |bytes| {
        String::from_utf8(bytes)
            .expect("footnotes UTF-8")
            .replace("FN {{fn}} {{rt}} {{lst}}", "FN {{fn}}")
            .into_bytes()
    });

    let output = DocxTemplate::open(&variant)
        .expect("open the multi-section footnotes variant")
        .render(
            &json!({"fn": "{{ second }}", "second": "SECOND-PASS"}),
            &RenderOptions::compat(),
        )
        .expect("render the multi-section footnotes")
        .to_bytes()
        .expect("serialize the output");
    let footnotes = String::from_utf8(zip_part_from_bytes(&output, "word/footnotes.xml"))
        .expect("output footnotes are UTF-8");
    assert!(footnotes.contains("{{ second }}"), "{footnotes}");
    assert!(!footnotes.contains("SECOND-PASS"), "{footnotes}");
}
