//! Minimal DOCX integration regressions for the publicly documented P6 rejection boundaries.

mod test_support;

use std::fs::File;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use docxtpl_rs::{DocxTemplate, RenderContext, RenderOptions, RenderValue};
use zip::write::{SimpleFileOptions, ZipWriter};
use zip::{CompressionMethod, ZipArchive};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn rewrite_document(src: &Path, dst: &Path, fragment: &str) {
    let mut archive =
        ZipArchive::new(File::open(src).expect("open the source docx")).expect("read the zip");
    let out = File::create(dst).expect("create the variant docx");
    let mut writer = ZipWriter::new(out);
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("read entry");
        let name = entry.name().to_string();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).expect("read entry bytes");
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
            .expect("write entry");
        writer.write_all(&bytes).expect("write entry bytes");
    }
    writer.finish().expect("finish the variant zip");
}

fn add_custom_properties(src: &Path, dst: &Path) {
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
            .expect("write entry");
        writer.write_all(&bytes).expect("write entry bytes");
    }
    writer
        .start_file("docProps/custom.xml", SimpleFileOptions::default())
        .expect("write the custom properties part");
    writer
        .write_all(br#"<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/custom-properties"/>"#)
        .expect("write the custom properties bytes");
    writer.finish().expect("finish the variant zip");
}

fn assert_subdoc_rejected(fragment: &str, expected: &str) {
    let workspace_target = test_support::target_dir();
    let dir = tempfile::Builder::new()
        .prefix("p6-boundary-")
        .tempdir_in(workspace_target)
        .expect("create a tempdir in the project target");
    let base = root().join("tests/fixtures/templates/p6_subdoc_basic_sub.docx");
    let variant = dir.path().join("sub.docx");
    rewrite_document(&base, &variant, fragment);

    let main = DocxTemplate::open(root().join("tests/fixtures/templates/p6_subdoc_basic.docx"))
        .expect("open the main template");
    let mut session = main
        .render_session(&RenderOptions::compat())
        .expect("create the session");
    let err = session
        .new_subdoc(&variant)
        .expect_err("the boundary must be rejected");
    assert!(err.to_string().contains(expected), "actual error: {err}");
}

fn assert_subdoc_accepted(fragment: &str) {
    let workspace_target = test_support::target_dir();
    let dir = tempfile::Builder::new()
        .prefix("p6-compatible-")
        .tempdir_in(workspace_target)
        .expect("create a tempdir in the project target");
    let base = root().join("tests/fixtures/templates/p6_subdoc_basic_sub.docx");
    let variant = dir.path().join("sub.docx");
    rewrite_document(&base, &variant, fragment);

    let main = DocxTemplate::open(root().join("tests/fixtures/templates/p6_subdoc_basic.docx"))
        .expect("open the main template");
    let mut session = main
        .render_session(&RenderOptions::compat())
        .expect("create the session");
    session
        .new_subdoc(&variant)
        .expect("a compatible Subdoc path should succeed");
}

fn finish_subdoc(session: docxtpl_rs::RenderSession, value: RenderValue) -> (String, Vec<u8>) {
    let fragment = match &value {
        RenderValue::Subdoc(fragment) => fragment.as_str().to_owned(),
        other => panic!("new_subdoc returned unexpected value: {other:?}"),
    };
    let mut context = RenderContext::new();
    context.insert("sd", value);
    let output = session
        .finish(&context)
        .expect("render merged Subdoc")
        .to_bytes()
        .expect("serialize merged document");
    (fragment, output)
}

fn new_session() -> docxtpl_rs::RenderSession {
    DocxTemplate::open(root().join("tests/fixtures/templates/p6_subdoc_basic.docx"))
        .expect("open the main template")
        .render_session(&RenderOptions::compat())
        .expect("create the session")
}

#[test]
fn subdoc_reader_and_bytes_entries_match_path_entry() {
    let sub = root().join("tests/fixtures/templates/p6_subdoc_basic_sub.docx");
    let bytes = std::fs::read(&sub).expect("read the subdocument bytes");

    let mut path_session = new_session();
    let path_value = path_session
        .new_subdoc(&sub)
        .expect("the path entry merges the subdocument");
    let (expected_fragment, expected_output) = finish_subdoc(path_session, path_value);

    let mut reader_session = new_session();
    let reader_value = reader_session
        .new_subdoc_from_reader(Cursor::new(bytes.as_slice()))
        .expect("the reader entry merges the subdocument");
    let (reader_fragment, reader_output) = finish_subdoc(reader_session, reader_value);

    let mut bytes_session = new_session();
    let bytes_value = bytes_session
        .new_subdoc_from_bytes(&bytes)
        .expect("the bytes entry merges the subdocument");
    let (bytes_fragment, bytes_output) = finish_subdoc(bytes_session, bytes_value);

    assert_eq!(reader_fragment, expected_fragment);
    assert_eq!(bytes_fragment, expected_fragment);
    assert_eq!(reader_output, expected_output);
    assert_eq!(bytes_output, expected_output);
}

#[test]
fn rejects_smartart_reference() {
    assert_subdoc_rejected(
        r#"<w:p xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><dgm:relIds r:dm="rId999"/></w:p>"#,
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
        "footnotes part",
    );
}

#[test]
fn accepts_when_both_documents_have_multiple_sections() {
    let workspace_target = test_support::target_dir();
    let dir = tempfile::Builder::new()
        .prefix("p6-sections-")
        .tempdir_in(workspace_target)
        .expect("create a tempdir in the project target");
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
    let main = DocxTemplate::open(&main_variant).expect("open the multi-section main template");
    let mut session = main
        .render_session(&RenderOptions::compat())
        .expect("create the session");
    session
        .new_subdoc(&sub_variant)
        .expect("multi-section on both sides should succeed after fixing section types");
}

#[test]
fn ignores_nonstandard_document_scoped_custom_properties_part() {
    let dir = tempfile::Builder::new()
        .prefix("p6-custom-props-")
        .tempdir_in(test_support::target_dir())
        .expect("create a tempdir in the project target");
    let variant = dir.path().join("sub.docx");
    add_custom_properties(
        &root().join("tests/fixtures/templates/p6_subdoc_basic_sub.docx"),
        &variant,
    );
    let main = DocxTemplate::open(root().join("tests/fixtures/templates/p6_subdoc_basic.docx"))
        .expect("open the main template");
    let mut session = main
        .render_session(&RenderOptions::compat())
        .expect("create the session");
    session
        .new_subdoc(&variant)
        .expect("python-docx only reads custom properties from package-root relationships");
}

#[test]
fn accepts_nsid_remapping_path() {
    assert_subdoc_accepted(
        r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>numbered</w:t></w:r></w:p>"#,
    );
}
