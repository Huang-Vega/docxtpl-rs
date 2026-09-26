//! Package-level regressions for missing core-properties parts and encoding errors in P5.

use std::fs::File;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use docxtpl_rs::{DocxTemplate, Error, RenderContext, RenderOptions};
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
    let mut archive =
        ZipArchive::new(File::open(src).expect("open the source docx")).expect("read the zip");
    let mut writer = ZipWriter::new(File::create(dst).expect("create the variant docx"));
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("read entry");
        let name = entry.name().to_string();
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

fn zip_part(bytes: &[u8], name: &str) -> Vec<u8> {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).expect("read the output zip");
    let mut part = archive.by_name(name).expect("output part exists");
    let mut contents = Vec::new();
    part.read_to_end(&mut contents)
        .expect("read the output part");
    contents
}

#[test]
fn missing_core_properties_part_is_recreated_like_python_docx() {
    let dir = tempfile::Builder::new()
        .prefix("p5-core-missing-")
        .tempdir_in(root().join("target"))
        .expect("create a tempdir in the project target");
    let src = root().join("tests/fixtures/templates/p5_hf_basic.docx");
    let variant = dir.path().join("template.docx");
    rewrite_docx(&src, &variant, |name, bytes| {
        if name == CORE_PART {
            return None;
        }
        let xml = match name {
            "_rels/.rels" => String::from_utf8(bytes).expect("root rels is UTF-8").replace(
                &format!(
                    r#"<Relationship Id="rId3" Type="{CORE_REL_TYPE}" Target="docProps/core.xml"/>"#
                ),
                "",
            ),
            "[Content_Types].xml" => String::from_utf8(bytes).expect("Content Types is UTF-8").replace(
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
        .expect("a valid template without core properties should open")
        .render(&json!({}), &RenderOptions::compat())
        .expect("upstream creates a default core-properties part")
        .to_bytes()
        .expect("serialize the output");

    let core = String::from_utf8(zip_part(&output, CORE_PART)).expect("core properties are UTF-8");
    assert!(
        core.contains("<dc:title>Word Document</dc:title>"),
        "{core}"
    );
    assert!(
        core.contains(
            "<cp:lastModifiedBy xmlns:cp=\"http://schemas.openxmlformats.org/officeDocument/2006/custom-properties\">python-docx</cp:lastModifiedBy>"
        ),
        "docxtpl 0.20.2 + docxcompose 2.2.0 rebinds the cp prefix of this child node: {core}"
    );

    let rels = String::from_utf8(zip_part(&output, "_rels/.rels")).expect("root rels is UTF-8");
    assert!(rels.contains(CORE_REL_TYPE), "{rels}");
    assert!(rels.contains("Target=\"docProps/core.xml\""), "{rels}");

    let content_types = String::from_utf8(zip_part(&output, "[Content_Types].xml"))
        .expect("Content Types is UTF-8");
    assert!(content_types.contains(CORE_CONTENT_TYPE), "{content_types}");
}

#[test]
fn non_utf8_core_properties_reports_part_name() {
    let dir = tempfile::Builder::new()
        .prefix("p5-core-encoding-")
        .tempdir_in(root().join("target"))
        .expect("create a tempdir in the project target");
    let src = root().join("tests/fixtures/templates/p5_hf_basic.docx");
    let variant = dir.path().join("template.docx");
    rewrite_docx(&src, &variant, |name, mut bytes| {
        if name == CORE_PART {
            bytes.push(0xff);
        }
        Some(bytes)
    });

    let result = DocxTemplate::open(&variant)
        .expect("the OPC layer does not pre-decode XML parts")
        .render(&json!({}), &RenderOptions::compat());
    let error = match result {
        Ok(_) => panic!("non-UTF-8 core properties must fail"),
        Err(error) => error,
    };
    assert!(matches!(
        &error,
        Error::NotUtf8 { part, .. } if part == CORE_PART
    ));
    assert!(error.to_string().contains(CORE_PART), "{error}");
}

#[test]
fn rust_environment_configuration_reaches_document_and_core_for_both_entry_points() {
    let dir = tempfile::Builder::new()
        .prefix("p5-custom-environment-")
        .tempdir_in(root().join("target"))
        .expect("create a tempdir in the project target");
    let src = root().join("tests/fixtures/templates/p5_hf_basic.docx");
    let variant = dir.path().join("template.docx");
    rewrite_docx(&src, &variant, |name, bytes| {
        let template = concat!(
            "{{ btitle|rust_tag }} ",
            "{{ rust_value() }} ",
            "{{ btitle is rust_named }}"
        );
        let xml = match name {
            "word/document.xml" => String::from_utf8(bytes)
                .expect("document UTF-8")
                .replace("{{btitle}}", template),
            CORE_PART => String::from_utf8(bytes)
                .expect("core UTF-8")
                .replace("<dc:title/>", &format!("<dc:title>{template}</dc:title>")),
            _ => return Some(bytes),
        };
        Some(xml.into_bytes())
    });

    let options = RenderOptions::compat().with_environment_configurator(|environment| {
        environment.add_filter("rust_tag", |value: String| format!("F_{value}"));
        environment.add_function("rust_value", || "FN".to_string());
        environment.add_test("rust_named", |value: String| value == "Ada");
    });
    let template = DocxTemplate::open(&variant).expect("open the custom-environment template");
    let direct = template
        .render(&json!({"btitle": "Ada"}), &options)
        .expect("DocxTemplate::render uses the custom environment");

    let context = RenderContext::try_from_json(&json!({"btitle": "Ada"})).expect("object context");
    let session = template
        .render_session(&options)
        .expect("create the RenderSession")
        .finish(&context)
        .expect("RenderSession::finish uses the custom environment");

    for rendered in [direct, session] {
        let output = rendered.to_bytes().expect("serialize the output");
        let document =
            String::from_utf8(zip_part(&output, "word/document.xml")).expect("document UTF-8");
        let core = String::from_utf8(zip_part(&output, CORE_PART)).expect("core UTF-8");
        assert!(document.contains("BODY F_Ada FN True"), "{document}");
        assert!(
            core.contains("<dc:title>F_Ada FN True</dc:title>"),
            "{core}"
        );
    }
}
