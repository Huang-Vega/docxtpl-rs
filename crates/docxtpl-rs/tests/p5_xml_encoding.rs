//! Python/lxml accepts BOM-tagged Unicode XmlPart inputs and serializes them as UTF-8.

mod test_support;

use std::fs::File;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use docxtpl_rs::{DocxTemplate, Error, RenderOptions};
use serde_json::json;
use zip::write::{SimpleFileOptions, ZipWriter};
use zip::{CompressionMethod, ZipArchive};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[derive(Clone, Copy)]
enum WireEncoding {
    Utf8Bom,
    Utf16 { little_endian: bool, bom: bool },
    Utf32 { little_endian: bool, bom: bool },
}

const XML_PARTS: [&str; 4] = [
    "word/document.xml",
    "word/header1.xml",
    "word/footer1.xml",
    "docProps/core.xml",
];

fn xml_with_encoding_declaration(xml: &[u8], label: &str) -> String {
    let text = String::from_utf8(xml.to_vec()).expect("fixture XmlPart is UTF-8");
    let replaced = text
        .replace("encoding='UTF-8'", &format!("encoding='{label}'"))
        .replace("encoding=\"UTF-8\"", &format!("encoding=\"{label}\""));
    assert_ne!(replaced, text, "fixture XmlPart has an XML declaration");
    replaced
}

fn encode_utf16_with_label(xml: &[u8], little_endian: bool, bom: bool, label: &str) -> Vec<u8> {
    let text = xml_with_encoding_declaration(xml, label);
    let mut bytes = Vec::with_capacity(text.len() * 2 + usize::from(bom) * 2);
    if bom {
        bytes.extend_from_slice(if little_endian {
            &[0xff, 0xfe]
        } else {
            &[0xfe, 0xff]
        });
    }
    for unit in text.encode_utf16() {
        let encoded = if little_endian {
            unit.to_le_bytes()
        } else {
            unit.to_be_bytes()
        };
        bytes.extend_from_slice(&encoded);
    }
    bytes
}

fn encode_utf16(xml: &[u8], little_endian: bool, bom: bool) -> Vec<u8> {
    let label = match (little_endian, bom) {
        (_, true) => "UTF-16",
        (true, false) => "UTF-16LE",
        (false, false) => "UTF-16BE",
    };
    encode_utf16_with_label(xml, little_endian, bom, label)
}

fn encode_utf32(xml: &[u8], little_endian: bool, bom: bool) -> Vec<u8> {
    let label = match (little_endian, bom) {
        (_, true) => "UTF-32",
        (true, false) => "UTF-32LE",
        (false, false) => "UTF-32BE",
    };
    let text = xml_with_encoding_declaration(xml, label);
    let mut bytes = Vec::with_capacity(text.len() * 4 + usize::from(bom) * 4);
    if bom {
        bytes.extend_from_slice(if little_endian {
            &[0xff, 0xfe, 0x00, 0x00]
        } else {
            &[0x00, 0x00, 0xfe, 0xff]
        });
    }
    for scalar in text.chars().map(u32::from) {
        let encoded = if little_endian {
            scalar.to_le_bytes()
        } else {
            scalar.to_be_bytes()
        };
        bytes.extend_from_slice(&encoded);
    }
    bytes
}

fn encode_xml(xml: &[u8], encoding: WireEncoding) -> Vec<u8> {
    match encoding {
        WireEncoding::Utf8Bom => {
            let mut encoded = Vec::with_capacity(xml.len() + 3);
            encoded.extend_from_slice(&[0xef, 0xbb, 0xbf]);
            encoded.extend_from_slice(xml);
            encoded
        }
        WireEncoding::Utf16 { little_endian, bom } => encode_utf16(xml, little_endian, bom),
        WireEncoding::Utf32 { little_endian, bom } => encode_utf32(xml, little_endian, bom),
    }
}

fn core_with_template(bytes: Vec<u8>) -> Vec<u8> {
    let xml = String::from_utf8(bytes).expect("core properties are UTF-8");
    let templated = xml.replace("<dc:title/>", "<dc:title>{{ core_title }}</dc:title>");
    assert_ne!(templated, xml, "fixture core.xml has an empty title");
    templated.into_bytes()
}

fn rewrite_docx(src: &Path, dst: &Path, mut transform: impl FnMut(&str, Vec<u8>) -> Vec<u8>) {
    let mut archive = ZipArchive::new(File::open(src).expect("open fixture")).expect("read zip");
    let mut writer = ZipWriter::new(File::create(dst).expect("create encoded fixture"));
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("read entry");
        let name = entry.name().to_string();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).expect("read entry bytes");
        bytes = transform(&name, bytes);
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
    writer.finish().expect("finish encoded fixture");
}

fn rewrite_known_xml_parts(src: &Path, dst: &Path, encodings: [WireEncoding; 4]) {
    rewrite_docx(src, dst, |name, mut bytes| {
        let Some(index) = XML_PARTS.iter().position(|candidate| *candidate == name) else {
            return bytes;
        };
        if name == "docProps/core.xml" {
            bytes = core_with_template(bytes);
        }
        encode_xml(&bytes, encodings[index])
    });
}

fn zip_part(docx: &[u8], name: &str) -> Vec<u8> {
    let mut archive = ZipArchive::new(Cursor::new(docx)).expect("read output zip");
    let mut part = archive.by_name(name).expect("output part exists");
    let mut bytes = Vec::new();
    part.read_to_end(&mut bytes).expect("read output part");
    bytes
}

fn render_encoded_variant(encodings: [WireEncoding; 4], marker: &str) -> Vec<u8> {
    let target = test_support::target_dir();
    let dir = tempfile::Builder::new()
        .prefix("p5-xml-encoding-")
        .tempdir_in(target)
        .expect("create project-local tempdir");
    let src = root().join("tests/fixtures/templates/p5_hf_basic.docx");
    let variant = dir.path().join("template.docx");
    rewrite_known_xml_parts(&src, &variant, encodings);

    let template = DocxTemplate::open(&variant).expect("OPC accepts encoded XmlParts");
    assert!(
        template
            .picture_map()
            .expect("inspect encoded stories")
            .is_empty(),
        "fixture has no pictures"
    );
    template
        .render(
            &json!({
                "btitle": format!("BODY-{marker}"),
                "htitle": format!("HEADER-{marker}"),
                "hshow": true,
                "hitems": [format!("ITEM-{marker}")],
                "ftitle": format!("FOOTER-{marker}"),
                "pageno": 7,
                "core_title": format!("CORE-{marker}")
            }),
            &RenderOptions::compat(),
        )
        .expect("render encoded XmlParts")
        .to_bytes()
        .expect("serialize output")
}

fn assert_utf8_rendered(output: &[u8], marker: &str) {
    let expected = [
        ("word/document.xml", format!("BODY-{marker}")),
        ("word/header1.xml", format!("HEADER-{marker}")),
        ("word/footer1.xml", format!("FOOTER-{marker}")),
        ("docProps/core.xml", format!("CORE-{marker}")),
    ];
    for (name, rendered_value) in expected {
        let bytes = zip_part(output, name);
        let text = std::str::from_utf8(&bytes)
            .unwrap_or_else(|error| panic!("{name} output is not UTF-8: {error}"));
        assert!(
            text.starts_with("<?xml version='1.0' encoding='UTF-8' standalone='yes'?>"),
            "{name}: {text}"
        );
        assert!(
            !bytes.contains(&0),
            "{name} still contains wide-encoding NUL bytes"
        );
        assert!(
            text.contains(&rendered_value),
            "{name} did not render {rendered_value:?}: {text}"
        );
    }
}

#[test]
fn legal_utf16_xml_parts_render_and_are_serialized_as_utf8() {
    let output = render_encoded_variant(
        [
            WireEncoding::Utf16 {
                little_endian: true,
                bom: true,
            },
            WireEncoding::Utf16 {
                little_endian: false,
                bom: true,
            },
            WireEncoding::Utf16 {
                little_endian: true,
                bom: false,
            },
            WireEncoding::Utf16 {
                little_endian: false,
                bom: false,
            },
        ],
        "UTF16",
    );
    assert_utf8_rendered(&output, "UTF16");
}

#[test]
fn legal_utf32_xml_parts_render_and_are_serialized_as_utf8() {
    let output = render_encoded_variant(
        [
            WireEncoding::Utf32 {
                little_endian: true,
                bom: true,
            },
            WireEncoding::Utf32 {
                little_endian: false,
                bom: true,
            },
            WireEncoding::Utf32 {
                little_endian: true,
                bom: false,
            },
            WireEncoding::Utf32 {
                little_endian: false,
                bom: false,
            },
        ],
        "UTF32",
    );
    assert_utf8_rendered(&output, "UTF32");
}

#[test]
fn utf8_bom_xml_parts_render_and_drop_the_bom() {
    let output = render_encoded_variant([WireEncoding::Utf8Bom; 4], "UTF8-BOM");
    assert_utf8_rendered(&output, "UTF8-BOM");
    for name in XML_PARTS {
        assert!(
            !zip_part(&output, name).starts_with(&[0xef, 0xbb, 0xbf]),
            "{name} retained its UTF-8 BOM"
        );
    }
}

fn render_invalid_document(prefix: &str, transform: impl FnOnce(Vec<u8>) -> Vec<u8>) -> Error {
    let target = test_support::target_dir();
    let dir = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(target)
        .expect("create project-local tempdir");
    let src = root().join("tests/fixtures/templates/p5_hf_basic.docx");
    let variant = dir.path().join("template.docx");
    let mut transform = Some(transform);
    rewrite_docx(&src, &variant, |name, bytes| {
        if name == "word/document.xml" {
            transform.take().expect("document part occurs once")(bytes)
        } else {
            bytes
        }
    });
    let result = DocxTemplate::open(&variant)
        .expect("OPC container remains valid")
        .render(&json!({}), &RenderOptions::compat());
    match result {
        Ok(_) => panic!("invalid XML encoding must be rejected"),
        Err(error) => error,
    }
}

fn assert_invalid_encoding(error: Error, expected_reason: &str) {
    match error {
        Error::InvalidXmlEncoding { part, reason } => {
            assert_eq!(part, "word/document.xml");
            assert!(reason.contains(expected_reason), "{reason}");
        }
        other => panic!("expected InvalidXmlEncoding, got {other:?}"),
    }
}

#[test]
fn odd_length_utf16_and_lone_surrogate_are_rejected() {
    let odd = render_invalid_document("p5-utf16-odd-", |bytes| {
        let mut encoded = encode_utf16(&bytes, true, true);
        encoded.pop();
        encoded
    });
    assert_invalid_encoding(odd, "odd");

    let lone_surrogate = render_invalid_document("p5-utf16-surrogate-", |bytes| {
        let mut encoded = encode_utf16(&bytes, true, true);
        let offset = encoded
            .windows(2)
            .position(|pair| pair == [b'B', 0])
            .expect("fixture contains BODY text");
        encoded[offset..offset + 2].copy_from_slice(&0xd800u16.to_le_bytes());
        encoded
    });
    assert_invalid_encoding(lone_surrogate, "surrogate");
}

#[test]
fn invalid_utf32_scalar_is_rejected() {
    let error = render_invalid_document("p5-utf32-scalar-", |bytes| {
        let mut encoded = encode_utf32(&bytes, true, true);
        let offset = encoded
            .windows(4)
            .position(|quad| quad == [b'B', 0, 0, 0])
            .expect("fixture contains BODY text");
        encoded[offset..offset + 4].copy_from_slice(&0xd800u32.to_le_bytes());
        encoded
    });
    assert_invalid_encoding(error, "U+0000D800");
}

#[test]
fn unknown_xml_encoding_declaration_is_rejected() {
    let error = render_invalid_document("p5-encoding-declaration-", |bytes| {
        encode_utf16_with_label(&bytes, true, true, "BOGUS")
    });
    assert_invalid_encoding(error, "BOGUS");
}

#[test]
fn utf8_bytes_with_a_wide_encoding_declaration_are_rejected() {
    let error = render_invalid_document("p5-encoding-mismatch-", |bytes| {
        xml_with_encoding_declaration(&bytes, "UTF-16").into_bytes()
    });
    assert_invalid_encoding(error, "actual bytes decode as UTF-8");
}

#[test]
fn footnotes_remain_utf8_only_like_upstream_generic_parts() {
    let target = test_support::target_dir();
    let dir = tempfile::Builder::new()
        .prefix("p5-footnotes-utf16-")
        .tempdir_in(target)
        .expect("create project-local tempdir");
    let src = root().join("tests/fixtures/templates/p5_footnotes_basic.docx");
    let variant = dir.path().join("template.docx");
    rewrite_docx(&src, &variant, |name, bytes| {
        if name == "word/footnotes.xml" {
            encode_utf16(&bytes, true, true)
        } else {
            bytes
        }
    });

    let result = DocxTemplate::open(&variant)
        .expect("OPC accepts the generic part bytes")
        .render(&json!({"fn": "note"}), &RenderOptions::compat());
    match result {
        Err(Error::NotUtf8 { part, .. }) => assert_eq!(part, "word/footnotes.xml"),
        Err(other) => panic!("expected footnotes NotUtf8, got {other:?}"),
        Ok(_) => panic!("UTF-16 generic footnotes part must be rejected"),
    }
}
