//! High-risk Subdoc compatibility paths verified against docxtpl 0.20.2 and
//! docxcompose 2.2.0 by `tests/oracle/probe_subdoc_highrisk.py`.
//!
//! The fixtures are assembled at runtime so every assertion is about one
//! isolated package behavior rather than an opaque checked-in binary.

mod test_support;

use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use docxtpl_opc::{relationships_path_of, PartUri, Relationships, TargetMode};
use docxtpl_rs::{DocxTemplate, RenderContext, RenderOptions, RenderValue};
use docxtpl_xml::{ns_uri, NodeId, NodeKind, XmlDocument, XmlLimits};
use zip::write::{SimpleFileOptions, ZipWriter};
use zip::{CompressionMethod, ZipArchive};

const NS_DGM: &str = "http://schemas.openxmlformats.org/drawingml/2006/diagram";
const RT_IMAGE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";
const RT_HYPERLINK: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink";
const RT_FOOTNOTES: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes";
const RT_CUSTOM_PROPERTIES: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/custom-properties";
const CT_FOOTNOTES: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml";
const CT_CUSTOM_PROPERTIES: &str =
    "application/vnd.openxmlformats-officedocument.custom-properties+xml";

const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x04, 0x00, 0x00, 0x00, 0xb5, 0x1c, 0x0c,
    0x02, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x64, 0xf8, 0x0f, 0x00,
    0x01, 0x05, 0x01, 0x01, 0x27, 0x18, 0xe3, 0x66, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44,
    0xae, 0x42, 0x60, 0x82,
];
const PNG_ALT: &[u8] = include_bytes!("../../../tests/fixtures/media/p4_dot2x1.png");

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn main_fixture() -> PathBuf {
    root().join("tests/fixtures/templates/p6_subdoc_basic.docx")
}

fn sub_fixture() -> PathBuf {
    root().join("tests/fixtures/templates/p6_subdoc_basic_sub.docx")
}

fn tempdir(prefix: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(test_support::target_dir())
        .expect("create temporary directory below the project target directory")
}

fn rewrite_docx(
    src: &Path,
    dst: &Path,
    mut transform: impl FnMut(&str, Vec<u8>) -> Option<Vec<u8>>,
    additions: &[(&str, &[u8])],
) {
    let mut archive =
        ZipArchive::new(File::open(src).expect("open source DOCX")).expect("read source ZIP");
    let mut writer = ZipWriter::new(File::create(dst).expect("create variant DOCX"));
    let mut source_names = HashSet::new();

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("read ZIP entry");
        let name = entry.name().to_owned();
        source_names.insert(name.clone());
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).expect("read ZIP entry bytes");
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
            .expect("write source ZIP entry");
        writer.write_all(&bytes).expect("write source entry bytes");
    }

    for &(name, bytes) in additions {
        assert!(
            !source_names.contains(name),
            "runtime fixture addition already exists: {name}"
        );
        writer
            .start_file(
                name,
                SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
            )
            .expect("write added ZIP entry");
        writer.write_all(bytes).expect("write added entry bytes");
    }
    writer.finish().expect("finish variant DOCX");
}

fn insert_before(xml: String, closing: &str, fragment: &str) -> String {
    let at = xml
        .find(closing)
        .unwrap_or_else(|| panic!("fixture has no {closing}"));
    let mut result = xml;
    result.insert_str(at, fragment);
    result
}

fn inject_before_final_sect_pr(mut xml: String, fragment: &str) -> String {
    let at = xml
        .rfind("<w:sectPr")
        .expect("fixture has a final body sectPr");
    xml.insert_str(at, fragment);
    xml
}

fn part_bytes(docx: &Path, name: &str) -> Option<Vec<u8>> {
    let mut archive =
        ZipArchive::new(File::open(docx).expect("open result DOCX")).expect("read result ZIP");
    let mut entry = archive.by_name(name).ok()?;
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes).expect("read result part");
    Some(bytes)
}

fn part_text(docx: &Path, name: &str) -> String {
    String::from_utf8(part_bytes(docx, name).unwrap_or_else(|| panic!("missing part {name}")))
        .unwrap_or_else(|_| panic!("part {name} is not UTF-8"))
}

fn part_names(docx: &Path) -> HashSet<String> {
    let mut archive =
        ZipArchive::new(File::open(docx).expect("open result DOCX")).expect("read result ZIP");
    (0..archive.len())
        .map(|index| {
            archive
                .by_index(index)
                .expect("read result ZIP entry")
                .name()
                .to_owned()
        })
        .collect()
}

fn parse_part(docx: &Path, name: &str) -> XmlDocument {
    XmlDocument::parse_strict(&part_text(docx, name), &XmlLimits::default())
        .unwrap_or_else(|e| panic!("parse {name}: {e}"))
}

fn elements_named(doc: &XmlDocument, ns: &str, local: &str) -> Vec<NodeId> {
    doc.descendants(doc.root())
        .into_iter()
        .filter(|&node| {
            doc.tag(node)
                .is_some_and(|tag| tag.ns == ns && tag.local == local)
        })
        .collect()
}

fn text_content(doc: &XmlDocument, node: NodeId) -> String {
    doc.descendants(node)
        .into_iter()
        .filter(|&child| doc.node_kind(child) == NodeKind::Text)
        .map(|child| doc.node_value(child))
        .collect()
}

fn render_subdoc(main: &Path, sub: &Path, output: &Path) -> String {
    let template = DocxTemplate::open(main).expect("open main template");
    let mut session = template
        .render_session(&RenderOptions::compat())
        .expect("create render session");
    let value = session.new_subdoc(sub).expect("compose subdocument");
    let fragment = match &value {
        RenderValue::Subdoc(fragment) => fragment.as_str().to_owned(),
        other => panic!("new_subdoc returned an unexpected value: {other:?}"),
    };
    let mut context = RenderContext::new();
    context.insert("sd", value);
    session
        .finish(&context)
        .expect("render composed document")
        .save(output)
        .expect("save composed document");
    fragment
}

fn relationships(docx: &Path, name: &str) -> Relationships {
    Relationships::parse(&part_text(docx, name))
        .unwrap_or_else(|e| panic!("parse relationships {name}: {e}"))
}

fn make_vml_subdoc(destination: &Path) {
    let fragment = r#"<w:p><w:r><w:pict><v:shape xmlns:v="urn:schemas-microsoft-com:vml" id="vml-oracle"><v:imagedata xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" r:id="rIdVML"/></v:shape></w:pict></w:r></w:p>"#;
    rewrite_docx(
        &sub_fixture(),
        destination,
        |name, bytes| {
            let xml = || String::from_utf8(bytes.clone()).expect("fixture XML is UTF-8");
            match name {
                "word/document.xml" => {
                    Some(inject_before_final_sect_pr(xml(), fragment).into_bytes())
                }
                "word/_rels/document.xml.rels" => Some(
                    insert_before(
                        xml(),
                        "</Relationships>",
                        &format!(
                            r#"<Relationship Id="rIdVML" Type="{RT_IMAGE}" Target="media/vml-oracle.png"/>"#
                        ),
                    )
                    .into_bytes(),
                ),
                "[Content_Types].xml" => Some(
                    insert_before(
                        xml(),
                        "</Types>",
                        r#"<Default Extension="png" ContentType="image/png"/>"#,
                    )
                    .into_bytes(),
                ),
                _ => Some(bytes),
            }
        },
        &[("word/media/vml-oracle.png", PNG_1X1)],
    );
}

#[test]
fn vml_imagedata_copies_an_image_relationship_and_media() {
    let dir = tempdir("subdoc-vml-");
    let sub = dir.path().join("sub.docx");
    let output = dir.path().join("output.docx");
    make_vml_subdoc(&sub);

    render_subdoc(&main_fixture(), &sub, &output);
    let document = parse_part(&output, "word/document.xml");
    let image_data = elements_named(&document, ns_uri::V, "imagedata");
    assert_eq!(image_data.len(), 1);
    let rid = document
        .attr(image_data[0], ns_uri::R, "id")
        .expect("VML image has r:id");
    let rels = relationships(&output, "word/_rels/document.xml.rels");
    let rel = rels.get(rid).expect("rewritten VML relationship exists");
    assert_eq!(rel.rel_type, RT_IMAGE);
    let owner = PartUri::new("word/document.xml").expect("valid owner URI");
    let target = rels
        .resolve(&owner, rid)
        .expect("VML relationship has an internal target");
    assert_eq!(
        part_bytes(&output, target.as_str()).as_deref(),
        Some(PNG_1X1)
    );
}

fn make_smartart_subdoc(destination: &Path) {
    let rel_ids = r#"<dgm:relIds xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" r:dm="rIdDgmData" r:lo="rIdDgmLayout" r:qs="rIdDgmStyle" r:cs="rIdDgmColors"/>"#;
    let fragment = format!("<w:p><w:r><w:drawing>{rel_ids}{rel_ids}</w:drawing></w:r></w:p>");
    let relationship_xml = r#"<Relationship Id="rIdDgmData" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/diagramData" Target="diagrams/data1.xml"/><Relationship Id="rIdDgmLayout" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/diagramLayout" Target="diagrams/layout1.xml"/><Relationship Id="rIdDgmStyle" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/diagramQuickStyle" Target="diagrams/quickStyle1.xml"/><Relationship Id="rIdDgmColors" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/diagramColors" Target="diagrams/colors1.xml"/>"#.to_string();
    let content_types = r#"<Override PartName="/word/diagrams/data1.xml" ContentType="application/vnd.openxmlformats-officedocument.drawingml.diagramData+xml"/><Override PartName="/word/diagrams/layout1.xml" ContentType="application/vnd.openxmlformats-officedocument.drawingml.diagramLayout+xml"/><Override PartName="/word/diagrams/quickStyle1.xml" ContentType="application/vnd.openxmlformats-officedocument.drawingml.diagramStyle+xml"/><Override PartName="/word/diagrams/colors1.xml" ContentType="application/vnd.openxmlformats-officedocument.drawingml.diagramColors+xml"/>"#;
    rewrite_docx(
        &sub_fixture(),
        destination,
        |name, bytes| {
            let xml = || String::from_utf8(bytes.clone()).expect("fixture XML is UTF-8");
            match name {
                "word/document.xml" => Some(
                    inject_before_final_sect_pr(xml(), &fragment).into_bytes(),
                ),
                "word/_rels/document.xml.rels" => Some(
                    insert_before(xml(), "</Relationships>", &relationship_xml).into_bytes(),
                ),
                "[Content_Types].xml" => {
                    Some(insert_before(xml(), "</Types>", content_types).into_bytes())
                }
                _ => Some(bytes),
            }
        },
        &[
            (
                "word/diagrams/data1.xml",
                br#"<dgm:dataModel xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram"/>"#,
            ),
            (
                "word/diagrams/layout1.xml",
                br#"<dgm:layoutDef xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" uniqueId="oracle-layout"/>"#,
            ),
            (
                "word/diagrams/quickStyle1.xml",
                br#"<dgm:styleDef xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" uniqueId="oracle-style"/>"#,
            ),
            (
                "word/diagrams/colors1.xml",
                br#"<dgm:colorsDef xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" uniqueId="oracle-colors"/>"#,
            ),
        ],
    );
}

#[test]
fn smartart_copies_four_parts_and_reuses_duplicate_relationships() {
    let dir = tempdir("subdoc-smartart-");
    let sub = dir.path().join("sub.docx");
    let output = dir.path().join("output.docx");
    make_smartart_subdoc(&sub);

    render_subdoc(&main_fixture(), &sub, &output);
    let document = parse_part(&output, "word/document.xml");
    let rel_ids = elements_named(&document, NS_DGM, "relIds");
    assert_eq!(rel_ids.len(), 2);
    let rels = relationships(&output, "word/_rels/document.xml.rels");
    let owner = PartUri::new("word/document.xml").expect("valid owner URI");
    let names = part_names(&output);
    let expected = [
        ("dm", "diagramData"),
        ("lo", "diagramLayout"),
        ("qs", "diagramQuickStyle"),
        ("cs", "diagramColors"),
    ];
    let mut targets = HashSet::new();

    for (attribute, rel_type_suffix) in expected {
        let first = document
            .attr(rel_ids[0], ns_uri::R, attribute)
            .unwrap_or_else(|| panic!("first SmartArt relIds has r:{attribute}"));
        let second = document
            .attr(rel_ids[1], ns_uri::R, attribute)
            .unwrap_or_else(|| panic!("second SmartArt relIds has r:{attribute}"));
        assert_eq!(
            first, second,
            "duplicate SmartArt references reuse r:{attribute}"
        );
        let rel = rels.get(first).expect("SmartArt relationship exists");
        assert_eq!(
            rel.rel_type,
            format!(
                "http://schemas.openxmlformats.org/officeDocument/2006/relationships/{rel_type_suffix}"
            )
        );
        let target = rels
            .resolve(&owner, first)
            .expect("SmartArt relationship has an internal target");
        assert!(
            names.contains(target.as_str()),
            "missing {}",
            target.as_str()
        );
        targets.insert(target.as_str().to_owned());
    }
    assert_eq!(
        targets.len(),
        4,
        "the four relationship roles keep distinct parts"
    );
}

fn make_mixed_media_subdoc(destination: &Path) {
    let fragment = concat!(
        r#"<w:p><w:r><w:drawing>"#,
        r#"<a:blip xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" "#,
        r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" r:embed="rIdOrdinary"/>"#,
        r#"<dgm:relIds xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" "#,
        r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" "#,
        r#"r:dm="rIdDgmData" r:lo="rIdDgmLayout" r:qs="rIdDgmStyle" r:cs="rIdDgmColors"/>"#,
        r#"</w:drawing><w:pict><v:shape xmlns:v="urn:schemas-microsoft-com:vml" id="mixed-vml">"#,
        r#"<v:imagedata xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" "#,
        r#"r:id="rIdVML"/></v:shape></w:pict></w:r></w:p>"#,
    );
    let relationship_xml = format!(
        concat!(
            r#"<Relationship Id="rIdOrdinary" Type="{0}" Target="media/image1.png"/>"#,
            r#"<Relationship Id="rIdVML" Type="{0}" Target="media/image2.png"/>"#,
            r#"<Relationship Id="rIdDgmData" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/diagramData" Target="diagrams/data1.xml"/>"#,
            r#"<Relationship Id="rIdDgmLayout" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/diagramLayout" Target="diagrams/layout1.xml"/>"#,
            r#"<Relationship Id="rIdDgmStyle" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/diagramQuickStyle" Target="diagrams/quickStyle1.xml"/>"#,
            r#"<Relationship Id="rIdDgmColors" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/diagramColors" Target="diagrams/colors1.xml"/>"#,
        ),
        RT_IMAGE,
    );
    let content_types = concat!(
        r#"<Default Extension="png" ContentType="image/png"/>"#,
        r#"<Override PartName="/word/diagrams/data1.xml" ContentType="application/vnd.openxmlformats-officedocument.drawingml.diagramData+xml"/>"#,
        r#"<Override PartName="/word/diagrams/layout1.xml" ContentType="application/vnd.openxmlformats-officedocument.drawingml.diagramLayout+xml"/>"#,
        r#"<Override PartName="/word/diagrams/quickStyle1.xml" ContentType="application/vnd.openxmlformats-officedocument.drawingml.diagramStyle+xml"/>"#,
        r#"<Override PartName="/word/diagrams/colors1.xml" ContentType="application/vnd.openxmlformats-officedocument.drawingml.diagramColors+xml"/>"#,
    );
    let data_xml = concat!(
        r#"<dgm:dataModel xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" "#,
        r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" "#,
        r#"xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">"#,
        r#"<dgm:extLst><a:blip r:embed="customRel"/></dgm:extLst></dgm:dataModel>"#,
    );
    let data_rels = format!(
        concat!(
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
            r#"<Relationship Id="customRel" Type="{}" Target="../media/image2.png"/>"#,
            r#"</Relationships>"#,
        ),
        RT_IMAGE,
    );

    rewrite_docx(
        &sub_fixture(),
        destination,
        |name, bytes| {
            let xml = || String::from_utf8(bytes.clone()).expect("fixture XML is UTF-8");
            match name {
                "word/document.xml" => {
                    Some(inject_before_final_sect_pr(xml(), fragment).into_bytes())
                }
                "word/_rels/document.xml.rels" => Some(
                    insert_before(xml(), "</Relationships>", &relationship_xml).into_bytes(),
                ),
                "[Content_Types].xml" => {
                    Some(insert_before(xml(), "</Types>", content_types).into_bytes())
                }
                _ => Some(bytes),
            }
        },
        &[
            ("word/media/image1.png", PNG_1X1),
            ("word/media/image2.png", PNG_ALT),
            ("word/diagrams/data1.xml", data_xml.as_bytes()),
            (
                "word/diagrams/_rels/data1.xml.rels",
                data_rels.as_bytes(),
            ),
            (
                "word/diagrams/layout1.xml",
                br#"<dgm:layoutDef xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram"/>"#,
            ),
            (
                "word/diagrams/quickStyle1.xml",
                br#"<dgm:styleDef xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram"/>"#,
            ),
            (
                "word/diagrams/colors1.xml",
                br#"<dgm:colorsDef xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram"/>"#,
            ),
        ],
    );
}

#[test]
fn recursive_and_pending_subdoc_images_share_partname_and_sha1_state() {
    let dir = tempdir("subdoc-mixed-media-");
    let sub = dir.path().join("sub.docx");
    let output = dir.path().join("output.docx");
    make_mixed_media_subdoc(&sub);

    render_subdoc(&main_fixture(), &sub, &output);

    let names = part_names(&output);
    let mut media: Vec<_> = names
        .iter()
        .filter(|name| name.starts_with("word/media/image"))
        .cloned()
        .collect();
    media.sort();
    assert_eq!(
        media,
        ["word/media/image1.png", "word/media/image2.png"],
        "ordinary, recursive SmartArt, and VML paths share one imageN allocator"
    );
    assert_eq!(
        part_bytes(&output, "word/media/image1.png").as_deref(),
        Some(PNG_1X1)
    );
    assert_eq!(
        part_bytes(&output, "word/media/image2.png").as_deref(),
        Some(PNG_ALT)
    );

    let document = parse_part(&output, "word/document.xml");
    let document_rels = relationships(&output, "word/_rels/document.xml.rels");
    let document_owner = PartUri::new("word/document.xml").expect("valid document URI");
    let ordinary = elements_named(&document, ns_uri::A, "blip");
    assert_eq!(ordinary.len(), 1);
    let ordinary_rid = document
        .attr(ordinary[0], ns_uri::R, "embed")
        .expect("ordinary image has r:embed");
    let ordinary_target = document_rels
        .resolve(&document_owner, ordinary_rid)
        .expect("resolve ordinary image target");
    assert_eq!(ordinary_target.as_str(), "word/media/image1.png");

    let vml = elements_named(&document, ns_uri::V, "imagedata");
    assert_eq!(vml.len(), 1);
    let vml_rid = document
        .attr(vml[0], ns_uri::R, "id")
        .expect("VML image has r:id");
    let vml_target = document_rels
        .resolve(&document_owner, vml_rid)
        .expect("resolve VML image target");

    let rel_ids = elements_named(&document, NS_DGM, "relIds");
    assert_eq!(rel_ids.len(), 1);
    let data_rid = document
        .attr(rel_ids[0], ns_uri::R, "dm")
        .expect("SmartArt data has r:dm");
    let data_part = document_rels
        .resolve(&document_owner, data_rid)
        .expect("resolve copied SmartArt data part");
    let data_rels_name = relationships_path_of(&data_part);
    let copied_data_rels = relationships(&output, &data_rels_name);
    let copied_data = parse_part(&output, data_part.as_str());
    let nested_blips = elements_named(&copied_data, ns_uri::A, "blip");
    assert_eq!(nested_blips.len(), 1);
    let nested_rid = copied_data
        .attr(nested_blips[0], ns_uri::R, "embed")
        .expect("copied SmartArt blob retains its nested image rId");
    assert_eq!(nested_rid, "customRel");
    let nested_image = copied_data_rels
        .get(nested_rid)
        .expect("arbitrary source relationship Id is preserved after copying");
    assert_eq!(nested_image.rel_type, RT_IMAGE);
    let nested_target = copied_data_rels
        .resolve(&data_part, &nested_image.id)
        .expect("resolve copied SmartArt nested image");

    assert_eq!(nested_target.as_str(), "word/media/image2.png");
    assert_eq!(
        vml_target, nested_target,
        "recursive copy and pending VML image reuse the same sha1 part"
    );
}

fn make_cyclic_relationship_subdoc(destination: &Path) {
    const RT_CYCLE: &str = "https://example.test/relationships/cycle";
    let fragment = r#"<w:p><w:r><w:object xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" r:id="cycleRoot"/></w:r></w:p>"#;
    let rel_a = format!(
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="toB" Type="{RT_CYCLE}" Target="cycle2.xml"/></Relationships>"#
    );
    let rel_b = format!(
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="backToA" Type="{RT_CYCLE}" Target="cycle1.xml"/></Relationships>"#
    );
    rewrite_docx(
        &sub_fixture(),
        destination,
        |name, bytes| {
            let xml = || String::from_utf8(bytes.clone()).expect("fixture XML is UTF-8");
            match name {
                "word/document.xml" => {
                    Some(inject_before_final_sect_pr(xml(), fragment).into_bytes())
                }
                "word/_rels/document.xml.rels" => Some(
                    insert_before(
                        xml(),
                        "</Relationships>",
                        &format!(
                            r#"<Relationship Id="cycleRoot" Type="{RT_CYCLE}" Target="cycles/cycle1.xml"/>"#
                        ),
                    )
                    .into_bytes(),
                ),
                _ => Some(bytes),
            }
        },
        &[
            ("word/cycles/cycle1.xml", br#"<cycle name="a"/>"#),
            ("word/cycles/cycle2.xml", br#"<cycle name="b"/>"#),
            ("word/cycles/_rels/cycle1.xml.rels", rel_a.as_bytes()),
            ("word/cycles/_rels/cycle2.xml.rels", rel_b.as_bytes()),
        ],
    );
}

#[test]
fn recursive_part_copy_closes_relationship_cycles_without_recursing_forever() {
    let dir = tempdir("subdoc-cyclic-rels-");
    let sub = dir.path().join("sub.docx");
    let output = dir.path().join("output.docx");
    make_cyclic_relationship_subdoc(&sub);

    render_subdoc(&main_fixture(), &sub, &output);

    let document = parse_part(&output, "word/document.xml");
    let object = elements_named(&document, ns_uri::W, "object")
        .into_iter()
        .next()
        .expect("copied object");
    let root_rid = document
        .attr(object, ns_uri::R, "id")
        .expect("copied object relationship");
    let document_owner = PartUri::new("word/document.xml").expect("valid document URI");
    let copied_a = relationships(&output, "word/_rels/document.xml.rels")
        .resolve(&document_owner, root_rid)
        .expect("resolve first copied cycle part");

    let rels_a = relationships(&output, &relationships_path_of(&copied_a));
    let copied_b = rels_a
        .resolve(&copied_a, "toB")
        .expect("resolve second copied cycle part");
    let rels_b = relationships(&output, &relationships_path_of(&copied_b));
    let back_to_a = rels_b
        .resolve(&copied_b, "backToA")
        .expect("resolve cycle back edge");

    assert_eq!(back_to_a, copied_a);
    assert_ne!(copied_a, copied_b);
}

fn make_custom_properties_subdoc(destination: &Path) {
    let fragment = r#"<w:p><w:fldSimple w:instr=" DOCPROPERTY SimpleProp \* MERGEFORMAT "><w:r><w:t>simple cached value</w:t></w:r></w:fldSimple></w:p><w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r><w:r><w:instrText xml:space="preserve"> DOCPROPERTY ComplexProp \* MERGEFORMAT </w:instrText></w:r><w:r><w:fldChar w:fldCharType="separate"/></w:r><w:r><w:t>complex cached value</w:t></w:r><w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>"#;
    let custom = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/custom-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><property fmtid="{D5CDD505-2E9C-101B-9397-08002B2CF9AE}" pid="2" name="SimpleProp"><vt:lpwstr>simple property source</vt:lpwstr></property><property fmtid="{D5CDD505-2E9C-101B-9397-08002B2CF9AE}" pid="3" name="ComplexProp"><vt:lpwstr>complex property source</vt:lpwstr></property></Properties>"#;
    rewrite_docx(
        &sub_fixture(),
        destination,
        |name, bytes| {
            let xml = || String::from_utf8(bytes.clone()).expect("fixture XML is UTF-8");
            match name {
                "word/document.xml" => {
                    Some(inject_before_final_sect_pr(xml(), fragment).into_bytes())
                }
                "_rels/.rels" => Some(
                    insert_before(
                        xml(),
                        "</Relationships>",
                        &format!(
                            r#"<Relationship Id="rIdCustomProperties" Type="{RT_CUSTOM_PROPERTIES}" Target="docProps/custom.xml"/>"#
                        ),
                    )
                    .into_bytes(),
                ),
                "[Content_Types].xml" => Some(
                    insert_before(
                        xml(),
                        "</Types>",
                        &format!(
                            r#"<Override PartName="/docProps/custom.xml" ContentType="{CT_CUSTOM_PROPERTIES}"/>"#
                        ),
                    )
                    .into_bytes(),
                ),
                _ => Some(bytes),
            }
        },
        &[("docProps/custom.xml", custom)],
    );
}

#[test]
fn standard_root_custom_properties_are_dissolved_without_copying_the_part() {
    let dir = tempdir("subdoc-custom-properties-");
    let sub = dir.path().join("sub.docx");
    let output = dir.path().join("output.docx");
    make_custom_properties_subdoc(&sub);

    render_subdoc(&main_fixture(), &sub, &output);
    let document = parse_part(&output, "word/document.xml");
    let text: Vec<&str> = elements_named(&document, ns_uri::W, "t")
        .into_iter()
        .flat_map(|node| {
            document
                .children(node)
                .iter()
                .copied()
                .filter(|&child| document.node_kind(child) == NodeKind::Text)
                .map(|child| document.node_value(child))
        })
        .collect();
    assert!(text.contains(&"simple cached value"));
    assert!(text.contains(&"complex cached value"));
    assert!(elements_named(&document, ns_uri::W, "fldSimple").is_empty());
    assert!(elements_named(&document, ns_uri::W, "instrText").is_empty());

    assert!(
        part_names(&output)
            .iter()
            .all(|name| !name.starts_with("docProps/custom")),
        "the subdocument custom-properties part is not imported"
    );
    assert_eq!(
        relationships(&output, "_rels/.rels")
            .iter()
            .filter(|rel| rel.rel_type == RT_CUSTOM_PROPERTIES)
            .count(),
        0
    );
}

fn source_footnotes() -> &'static [u8] {
    br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote><w:footnote w:id="1"><w:p><w:hyperlink r:id="rId1"><w:r><w:t>linked source note</w:t></w:r></w:hyperlink></w:p></w:footnote></w:footnotes>"#
}

fn source_footnote_relationships() -> &'static [u8] {
    br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.test/source-note" TargetMode="External"/></Relationships>"#
}

fn make_footnote_subdoc(destination: &Path) {
    let fragment = r#"<w:p><w:r><w:t>sub note marker</w:t></w:r><w:r><w:footnoteReference w:id="1"/></w:r></w:p>"#;
    rewrite_docx(
        &sub_fixture(),
        destination,
        |name, bytes| {
            let xml = || String::from_utf8(bytes.clone()).expect("fixture XML is UTF-8");
            match name {
                "word/document.xml" => {
                    Some(inject_before_final_sect_pr(xml(), fragment).into_bytes())
                }
                "word/_rels/document.xml.rels" => Some(
                    insert_before(
                        xml(),
                        "</Relationships>",
                        &format!(
                            r#"<Relationship Id="rIdFootnotesOracle" Type="{RT_FOOTNOTES}" Target="footnotes.xml"/>"#
                        ),
                    )
                    .into_bytes(),
                ),
                "[Content_Types].xml" => Some(
                    insert_before(
                        xml(),
                        "</Types>",
                        &format!(
                            r#"<Override PartName="/word/footnotes.xml" ContentType="{CT_FOOTNOTES}"/>"#
                        ),
                    )
                    .into_bytes(),
                ),
                _ => Some(bytes),
            }
        },
        &[
            ("word/footnotes.xml", source_footnotes()),
            (
                "word/_rels/footnotes.xml.rels",
                source_footnote_relationships(),
            ),
        ],
    );
}

fn make_main_with_footnotes(destination: &Path) {
    let footnotes = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote><w:footnote w:id="2"><w:p><w:hyperlink r:id="rId9"><w:r><w:t>main existing note</w:t></w:r></w:hyperlink></w:p></w:footnote></w:footnotes>"#;
    let rels = br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId9" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink" Target="https://example.test/main-note" TargetMode="External"/></Relationships>"#;
    rewrite_docx(
        &main_fixture(),
        destination,
        |name, bytes| {
            let xml = || String::from_utf8(bytes.clone()).expect("fixture XML is UTF-8");
            match name {
                "word/_rels/document.xml.rels" => Some(
                    insert_before(
                        xml(),
                        "</Relationships>",
                        &format!(
                            r#"<Relationship Id="rIdFootnotesOracle" Type="{RT_FOOTNOTES}" Target="footnotes.xml"/>"#
                        ),
                    )
                    .into_bytes(),
                ),
                "[Content_Types].xml" => Some(
                    insert_before(
                        xml(),
                        "</Types>",
                        &format!(
                            r#"<Override PartName="/word/footnotes.xml" ContentType="{CT_FOOTNOTES}"/>"#
                        ),
                    )
                    .into_bytes(),
                ),
                _ => Some(bytes),
            }
        },
        &[
            ("word/footnotes.xml", footnotes),
            ("word/_rels/footnotes.xml.rels", rels),
        ],
    );
}

fn assert_footnote_copy(output: &Path, expected_id: &str) {
    let document = parse_part(output, "word/document.xml");
    let refs = elements_named(&document, ns_uri::W, "footnoteReference");
    assert_eq!(refs.len(), 1);
    assert_eq!(document.attr(refs[0], ns_uri::W, "id"), Some(expected_id));

    let document_rels = relationships(output, "word/_rels/document.xml.rels");
    let footnotes_rel = document_rels
        .iter()
        .find(|rel| rel.rel_type == RT_FOOTNOTES)
        .expect("document relates to the main footnotes part");
    let document_owner = PartUri::new("word/document.xml").expect("valid document URI");
    assert_eq!(
        document_rels
            .resolve(&document_owner, &footnotes_rel.id)
            .expect("resolve footnotes relationship")
            .as_str(),
        "word/footnotes.xml"
    );

    let footnotes = parse_part(output, "word/footnotes.xml");
    let copied = elements_named(&footnotes, ns_uri::W, "footnote")
        .into_iter()
        .find(|&node| text_content(&footnotes, node).contains("linked source note"))
        .expect("source footnote is copied");
    assert_eq!(footnotes.attr(copied, ns_uri::W, "id"), Some(expected_id));
    let hyperlinks: Vec<_> = footnotes
        .descendants(copied)
        .into_iter()
        .filter(|&node| {
            footnotes
                .tag(node)
                .is_some_and(|tag| tag.ns == ns_uri::W && tag.local == "hyperlink")
        })
        .collect();
    assert_eq!(hyperlinks.len(), 1);
    let rid = footnotes
        .attr(hyperlinks[0], ns_uri::R, "id")
        .expect("copied footnote hyperlink has r:id");
    let footnote_rels = relationships(output, "word/_rels/footnotes.xml.rels");
    let rel = footnote_rels
        .get(rid)
        .expect("copied footnote relationship exists");
    assert_eq!(rel.rel_type, RT_HYPERLINK);
    assert_eq!(rel.target, "https://example.test/source-note");
    assert_eq!(rel.target_mode, TargetMode::External);
}

#[test]
fn creates_footnotes_part_and_copies_id_and_relationship() {
    let dir = tempdir("subdoc-footnotes-new-");
    let sub = dir.path().join("sub.docx");
    let output = dir.path().join("output.docx");
    make_footnote_subdoc(&sub);

    render_subdoc(&main_fixture(), &sub, &output);
    assert_footnote_copy(&output, "1");
}

#[test]
fn appends_to_existing_footnotes_and_copies_id_and_relationship() {
    let dir = tempdir("subdoc-footnotes-existing-");
    let main = dir.path().join("main.docx");
    let sub = dir.path().join("sub.docx");
    let output = dir.path().join("output.docx");
    make_main_with_footnotes(&main);
    make_footnote_subdoc(&sub);

    render_subdoc(&main, &sub, &output);
    assert_footnote_copy(&output, "4");
}
