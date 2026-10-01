//! Subdoc compatibility regressions for behavior implemented by docxcompose.
//!
//! These tests intentionally build their DOCX variants at runtime.  The source
//! fixtures are ordinary Word files, while the mutations isolate one behavior
//! at a time and keep the assertions semantic (not byte-for-byte), notably for
//! docxcompose's randomly generated `w:nsid` values.

mod test_support;

use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use docxtpl_rs::{DocxTemplate, RenderContext, RenderOptions, RenderValue};
use docxtpl_xml::{ns_uri, NodeId, XmlDocument, XmlLimits};
use zip::write::{SimpleFileOptions, ZipWriter};
use zip::{CompressionMethod, ZipArchive};

const RT_NUMBERING: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering";
const CT_NUMBERING: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml";

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
        .expect("create temporary directory under the project target directory")
}

fn rewrite_docx(
    src: &Path,
    dst: &Path,
    mut transform: impl FnMut(&str, Vec<u8>) -> Option<Vec<u8>>,
) {
    let mut archive =
        ZipArchive::new(File::open(src).expect("open source DOCX")).expect("read source ZIP");
    let mut writer = ZipWriter::new(File::create(dst).expect("create variant DOCX"));
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("read ZIP entry");
        let name = entry.name().to_owned();
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
            .expect("write ZIP entry");
        writer.write_all(&bytes).expect("write ZIP entry bytes");
    }
    writer.finish().expect("finish variant DOCX");
}

fn rewrite_document(src: &Path, dst: &Path, mutate: impl FnOnce(String) -> String) {
    let mut mutate = Some(mutate);
    rewrite_docx(src, dst, |name, bytes| {
        if name != "word/document.xml" {
            return Some(bytes);
        }
        let xml = String::from_utf8(bytes).expect("document.xml is UTF-8");
        Some(mutate.take().expect("document mutator is called once")(xml).into_bytes())
    });
}

fn insert_before_final_sect_pr(mut xml: String, fragment: &str) -> String {
    let at = xml
        .rfind("<w:sectPr")
        .expect("fixture has a final body sectPr");
    xml.insert_str(at, fragment);
    xml
}

fn insert_before_placeholder(mut xml: String, fragment: &str) -> String {
    let marker = "<w:p><w:r><w:t>{{p sd }}";
    let at = xml
        .find(marker)
        .expect("fixture has the subdoc placeholder");
    xml.insert_str(at, fragment);
    xml
}

fn set_final_section_type(mut xml: String, value: &str) -> String {
    let at = xml
        .rfind("<w:sectPr")
        .expect("fixture has a final body sectPr");
    let open_end = at + xml[at..].find('>').expect("sectPr start tag closes") + 1;
    xml.insert_str(open_end, &format!(r#"<w:type w:val="{value}"/>"#));
    xml
}

fn strip_empty_element_containing(mut xml: String, needle: &str) -> String {
    let needle_at = xml.find(needle).expect("element marker exists");
    let start = xml[..needle_at]
        .rfind('<')
        .expect("marked element has a start tag");
    let end = needle_at
        + xml[needle_at..]
            .find("/>")
            .expect("marked element is empty")
        + 2;
    xml.replace_range(start..end, "");
    xml
}

fn remove_main_numbering(src: &Path, dst: &Path) {
    rewrite_docx(src, dst, |name, bytes| match name {
        "word/numbering.xml" => None,
        "word/_rels/document.xml.rels" => Some(
            strip_empty_element_containing(
                String::from_utf8(bytes).expect("document rels is UTF-8"),
                RT_NUMBERING,
            )
            .into_bytes(),
        ),
        "[Content_Types].xml" => Some(
            strip_empty_element_containing(
                String::from_utf8(bytes).expect("content types is UTF-8"),
                "PartName=\"/word/numbering.xml\"",
            )
            .into_bytes(),
        ),
        _ => Some(bytes),
    });
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

fn body(doc: &XmlDocument) -> NodeId {
    elements_named(doc, ns_uri::W, "body")
        .into_iter()
        .next()
        .expect("document has a body")
}

fn section_nodes(doc: &XmlDocument) -> Vec<NodeId> {
    let body = body(doc);
    let mut sections = Vec::new();
    for &child in doc.children(body) {
        if doc
            .tag(child)
            .is_some_and(|tag| tag.ns == ns_uri::W && tag.local == "p")
        {
            for &p_pr in doc.children(child) {
                if !doc
                    .tag(p_pr)
                    .is_some_and(|tag| tag.ns == ns_uri::W && tag.local == "pPr")
                {
                    continue;
                }
                sections.extend(doc.children(p_pr).iter().copied().filter(|&node| {
                    doc.tag(node)
                        .is_some_and(|tag| tag.ns == ns_uri::W && tag.local == "sectPr")
                }));
            }
        } else if doc
            .tag(child)
            .is_some_and(|tag| tag.ns == ns_uri::W && tag.local == "sectPr")
        {
            sections.push(child);
        }
    }
    sections
}

fn section_type(doc: &XmlDocument, sect_pr: NodeId) -> String {
    doc.children(sect_pr)
        .iter()
        .copied()
        .find(|&node| {
            doc.tag(node)
                .is_some_and(|tag| tag.ns == ns_uri::W && tag.local == "type")
        })
        .and_then(|node| doc.attr(node, ns_uri::W, "val"))
        .unwrap_or("nextPage")
        .to_owned()
}

fn numbered_paragraph() -> &'static str {
    r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>numbered import</w:t></w:r></w:p>"#
}

#[test]
fn accepts_fragment_prefixes_inherited_from_the_word_document_root() {
    let dir = tempdir("subdoc-extra-namespaces-");
    let sub = dir.path().join("sub.docx");
    let output = dir.path().join("output.docx");
    let fragment = r#"<w:p w14:paraId="ABCDEF12"><w:r><m:oMath><m:r><m:t>x</m:t></m:r></m:oMath><w14:checkbox><w14:checked w14:val="1"/></w14:checkbox><wp14:sizeRelH relativeFrom="margin"><wp14:pctWidth>50000</wp14:pctWidth></wp14:sizeRelH></w:r></w:p>"#;
    rewrite_document(&sub_fixture(), &sub, |xml| {
        insert_before_final_sect_pr(xml, fragment)
    });

    let composed_fragment = render_subdoc(&main_fixture(), &sub, &output);
    assert!(composed_fragment.contains("<m:oMath>"));
    assert!(composed_fragment.contains("w14:paraId=\"ABCDEF12\""));
    assert!(composed_fragment.contains("<wp14:sizeRelH"));

    let document = parse_part(&output, "word/document.xml");
    assert_eq!(elements_named(&document, ns_uri::M, "oMath").len(), 1);
    assert_eq!(elements_named(&document, ns_uri::W14, "checkbox").len(), 1);
    assert_eq!(elements_named(&document, ns_uri::WP14, "sizeRelH").len(), 1);
}

#[test]
fn creates_a_main_numbering_part_when_the_numbered_subdoc_needs_one() {
    let dir = tempdir("subdoc-create-numbering-");
    let main = dir.path().join("main.docx");
    let sub = dir.path().join("sub.docx");
    let output = dir.path().join("output.docx");
    remove_main_numbering(&main_fixture(), &main);
    rewrite_document(&sub_fixture(), &sub, |xml| {
        insert_before_final_sect_pr(xml, numbered_paragraph())
    });

    let fragment = render_subdoc(&main, &sub, &output);
    assert!(fragment.contains("<w:numId"));
    assert!(part_bytes(&output, "word/numbering.xml").is_some());
    assert!(
        part_text(&output, "word/_rels/document.xml.rels").contains(RT_NUMBERING),
        "the new numbering part must be related from document.xml"
    );
    let content_types = part_text(&output, "[Content_Types].xml");
    assert!(content_types.contains("PartName=\"/word/numbering.xml\""));
    assert!(content_types.contains(CT_NUMBERING));

    let numbering = parse_part(&output, "word/numbering.xml");
    assert!(!elements_named(&numbering, ns_uri::W, "abstractNum").is_empty());
    assert!(!elements_named(&numbering, ns_uri::W, "num").is_empty());
}

#[test]
fn copied_abstract_numbering_gets_a_valid_unique_nsid() {
    let dir = tempdir("subdoc-nsid-");
    let sub = dir.path().join("sub.docx");
    let output = dir.path().join("output.docx");
    rewrite_document(&sub_fixture(), &sub, |xml| {
        insert_before_final_sect_pr(xml, numbered_paragraph())
    });

    render_subdoc(&main_fixture(), &sub, &output);
    let numbering = parse_part(&output, "word/numbering.xml");
    let nsids: Vec<String> = elements_named(&numbering, ns_uri::W, "nsid")
        .into_iter()
        .map(|node| {
            numbering
                .attr(node, ns_uri::W, "val")
                .expect("nsid has w:val")
                .to_owned()
        })
        .collect();
    assert!(nsids.len() >= 10, "a numbered subdoc adds an abstractNum");
    assert!(nsids
        .iter()
        .all(|value| value.len() == 8 && value.chars().all(|ch| ch.is_ascii_hexdigit())));
    let unique: HashSet<&str> = nsids.iter().map(String::as_str).collect();
    assert_eq!(unique.len(), nsids.len(), "all nsid values must be unique");
}

#[test]
fn restarts_the_first_decimal_list_for_a_numbered_paragraph_style() {
    let dir = tempdir("subdoc-list-restart-");
    let sub = dir.path().join("sub.docx");
    let output = dir.path().join("output.docx");
    rewrite_document(&sub_fixture(), &sub, |xml| {
        insert_before_final_sect_pr(
            xml,
            r#"<w:p><w:pPr><w:pStyle w:val="ListNumber"/></w:pPr><w:r><w:t>restart me</w:t></w:r></w:p>"#,
        )
    });

    let fragment = render_subdoc(&main_fixture(), &sub, &output);
    let wrapped = format!(r#"<w:body xmlns:w="{}">{fragment}</w:body>"#, ns_uri::W);
    let fragment_doc = XmlDocument::parse_strict(&wrapped, &XmlLimits::default())
        .expect("parse returned subdoc fragment");
    let restarted_num_id = elements_named(&fragment_doc, ns_uri::W, "numId")
        .into_iter()
        .find_map(|node| fragment_doc.attr(node, ns_uri::W, "val"))
        .expect("the restarted paragraph gets an explicit numId")
        .to_owned();

    let numbering = parse_part(&output, "word/numbering.xml");
    let restarted_num = elements_named(&numbering, ns_uri::W, "num")
        .into_iter()
        .find(|&node| numbering.attr(node, ns_uri::W, "numId") == Some(&restarted_num_id))
        .expect("numbering.xml contains the restarted w:num");
    let overrides: Vec<_> = numbering
        .descendants(restarted_num)
        .into_iter()
        .filter(|&node| {
            numbering
                .tag(node)
                .is_some_and(|tag| tag.ns == ns_uri::W && tag.local == "lvlOverride")
        })
        .collect();
    assert_eq!(overrides.len(), 1);
    assert_eq!(numbering.attr(overrides[0], ns_uri::W, "ilvl"), Some("0"));
    let starts: Vec<_> = numbering
        .descendants(overrides[0])
        .into_iter()
        .filter(|&node| {
            numbering
                .tag(node)
                .is_some_and(|tag| tag.ns == ns_uri::W && tag.local == "startOverride")
        })
        .collect();
    assert_eq!(starts.len(), 1);
    assert_eq!(numbering.attr(starts[0], ns_uri::W, "val"), Some("1"));
}

#[test]
fn reconciles_section_start_types_when_both_documents_have_multiple_sections() {
    let dir = tempdir("subdoc-multi-section-");
    let main = dir.path().join("main.docx");
    let sub = dir.path().join("sub.docx");
    let output = dir.path().join("output.docx");
    rewrite_document(&main_fixture(), &main, |xml| {
        let xml = insert_before_placeholder(
            xml,
            r#"<w:p><w:pPr><w:sectPr><w:type w:val="continuous"/></w:sectPr></w:pPr><w:r><w:t>main section one</w:t></w:r></w:p>"#,
        );
        set_final_section_type(xml, "oddPage")
    });
    rewrite_document(&sub_fixture(), &sub, |xml| {
        let xml = insert_before_final_sect_pr(
            xml,
            r#"<w:p><w:pPr><w:sectPr><w:type w:val="evenPage"/></w:sectPr></w:pPr><w:r><w:t>sub section one</w:t></w:r></w:p>"#,
        );
        set_final_section_type(xml, "continuous")
    });

    render_subdoc(&main, &sub, &output);
    let document = parse_part(&output, "word/document.xml");
    let types: Vec<String> = section_nodes(&document)
        .into_iter()
        .map(|section| section_type(&document, section))
        .collect();
    assert_eq!(types, ["oddPage", "evenPage", "continuous"]);
}
