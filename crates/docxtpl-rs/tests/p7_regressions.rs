//! Non-oracle branch regressions for the P7 replacement family and picture map.

use std::fs::File;
use std::io::{Cursor, Read, Write};
use std::path::Path;
use std::path::PathBuf;

use docxtpl_opc::{Package, PackageLimits};
use docxtpl_rs::{DocxTemplate, InlineImage, RenderContext, RenderOptions, TemplateErrorKind};
use serde_json::json;
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
    .unwrap_or_else(|e| panic!("open {name}: {e}"))
}

fn media(name: &str) -> Vec<u8> {
    std::fs::read(root().join("tests/fixtures/media").join(name))
        .unwrap_or_else(|e| panic!("read {name}: {e}"))
}

fn zip_part(bytes: &[u8], name: &str) -> Vec<u8> {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).expect("read the output zip");
    let mut part = archive
        .by_name(name)
        .unwrap_or_else(|error| panic!("output is missing {name}: {error}"));
    let mut contents = Vec::new();
    part.read_to_end(&mut contents)
        .expect("read the output part");
    contents
}

fn rewrite_document(src: &Path, dst: &Path, from: &str, to: &str) {
    let mut archive =
        ZipArchive::new(File::open(src).expect("open the source docx")).expect("read the zip");
    let mut writer = ZipWriter::new(File::create(dst).expect("create the variant docx"));
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("read entry");
        let name = entry.name().to_string();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).expect("read entry bytes");
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
            .expect("write entry");
        writer.write_all(&bytes).expect("write entry bytes");
    }
    writer.finish().expect("finish the variant zip");
}

#[test]
fn picture_map_reports_relative_target() {
    let map = template("p7_media_body")
        .picture_map()
        .expect("read the picture map");
    assert_eq!(
        map.get("image.png").map(String::as_str),
        Some("media/image1.png")
    );
}

#[test]
fn public_json_render_rejects_non_object_contexts() {
    for context in [json!(null), json!([]), json!("text"), json!(1)] {
        let result = template("r2_var_basic").render(&context, &RenderOptions::compat());
        let error = match result {
            Ok(_) => panic!("top-level context {context} must be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), Some(TemplateErrorKind::InvalidArgument));
        assert!(error.to_string().contains("word/document.xml"), "{error}");
        assert!(
            error
                .to_string()
                .contains("top-level JSON render context must be an object"),
            "{error}"
        );
    }
}

#[test]
fn reset_replacements_clears_missing_picture_and_media_registration() {
    let tpl = template("p7_pic_missing");
    let mut session = tpl
        .render_session(&RenderOptions::compat())
        .expect("create the session");
    session
        .replace_pic("nope.png", media("p7_media3x2.png"))
        .replace_media(media("p7_dummy.png"), media("p7_new4x4.png"))
        .reset_replacements();
    session
        .finish(&RenderContext::new())
        .expect("no replacements or missing-picture errors may remain after reset");
}

#[test]
fn allow_missing_pics_is_opt_in_and_survives_reset() {
    let tpl = template("p7_pic_missing");

    let mut strict = tpl
        .render_session(&RenderOptions::compat())
        .expect("create the strict session");
    strict.replace_pic("nope.png", media("p7_media3x2.png"));
    let err = match strict.finish(&RenderContext::new()) {
        Ok(_) => panic!("unmatched picture keys should be rejected by default"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("Picture nope.png not found"));

    let mut lenient = tpl
        .render_session(&RenderOptions::compat())
        .expect("create the lenient session");
    lenient
        .allow_missing_pics(true)
        .replace_pic("discarded.png", media("p7_dummy.png"))
        .reset_replacements()
        .replace_pic("nope.png", media("p7_media3x2.png"));
    lenient
        .finish(&RenderContext::new())
        .expect("explicit lenient mode should ignore unmatched picture keys");
}

#[test]
fn allow_missing_pics_applies_without_rendering() {
    let tpl = template("p7_pic_missing");
    let mut session = tpl
        .render_session(&RenderOptions::compat())
        .expect("create the session");
    session
        .replace_pic("nope.png", media("p7_media3x2.png"))
        .allow_missing_pics(true);
    session
        .finish_without_render()
        .expect("saving without rendering should also apply the lenient missing-picture policy");
}

#[test]
fn finish_without_render_normalizes_real_word_xml_parts() {
    let output = template("p7b_hf_entities")
        .render_session(&RenderOptions::compat())
        .expect("create the session")
        .finish_without_render()
        .expect("a real Word template saved without rendering")
        .to_bytes()
        .expect("serialize the output");

    let declaration = b"<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n";
    for name in [
        "word/document.xml",
        "word/header1.xml",
        "word/footer1.xml",
        "docProps/core.xml",
        "word/styles.xml",
        "word/settings.xml",
    ] {
        let part = zip_part(&output, name);
        assert!(
            part.starts_with(declaration),
            "{name} should be re-serialized with the lxml declaration like python-docx save, actual prefix: {:?}",
            &part[..part.len().min(declaration.len())]
        );
    }
}

#[test]
fn regular_render_does_not_recanonicalize_generated_image_xml() {
    let image_path = root().join("tests/fixtures/media/p4_dot2x1.png");
    let image = InlineImage::from_path(
        image_path.to_str().expect("the image path is UTF-8"),
        None,
        None,
        None,
    )
    .expect("read the test image");
    let mut context = RenderContext::new();
    context.insert("img", image);
    let output = template("p4_img_png")
        .render_ctx(&context, &RenderOptions::compat())
        .expect("render the image template")
        .to_bytes()
        .expect("serialize the image output");
    let expected = std::fs::read(root().join("tests/oracle/expected/p4_img_png.docx"))
        .expect("read the Python oracle image output");

    assert_eq!(
        zip_part(&output, "word/document.xml"),
        zip_part(&expected, "word/document.xml"),
        "after a normal render, strip-blank-text must not run again; image XML whitespace and attributes must be preserved"
    );
}

#[test]
fn undeclared_variables_can_subtract_context_or_keys() {
    let tpl = template("p7_undeclared_vars");
    let all = tpl
        .undeclared_variables()
        .expect("read all undeclared variables");
    assert_eq!(
        all,
        ["a", "b", "c", "items", "vars"].map(String::from).into()
    );

    let without_keys = tpl
        .undeclared_variables_with_keys(["a", "items", "not_in_template"])
        .expect("subtract by keys");
    assert_eq!(without_keys, ["b", "c", "vars"].map(String::from).into());

    let mut context = RenderContext::new();
    context.insert("b", true).insert("vars", "existing value");
    let without_context = tpl
        .undeclared_variables_with_context(&context)
        .expect("subtract using the typed context");
    assert_eq!(
        without_context,
        ["a", "c", "items"].map(String::from).into()
    );
}

#[test]
fn one_template_can_render_multiple_independent_outputs() {
    let tpl = template("r2_var_basic");
    let first = tpl
        .render(&json!({"name": "First"}), &RenderOptions::compat())
        .expect("first render")
        .to_bytes()
        .expect("first output");
    let second = tpl
        .render(&json!({"name": "Second"}), &RenderOptions::compat())
        .expect("second render")
        .to_bytes()
        .expect("second output");

    let first_xml = String::from_utf8(zip_part(&first, "word/document.xml")).unwrap();
    let second_xml = String::from_utf8(zip_part(&second, "word/document.xml")).unwrap();
    assert!(first_xml.contains("First"));
    assert!(!first_xml.contains("Second"));
    assert!(second_xml.contains("Second"));
    assert!(!second_xml.contains("First"));
}

#[test]
fn reusable_path_template_invalidates_preprocessing_when_source_changes() {
    let workspace_target = root().join("target");
    let dir = tempfile::Builder::new()
        .prefix("render-cache-invalidation-")
        .tempdir_in(workspace_target)
        .expect("create a tempdir in the project target");
    let source = root().join("tests/fixtures/templates/r2_var_basic.docx");
    let variant = dir.path().join("template.docx");
    rewrite_document(&source, &variant, "{{name}}", "{{name}}");
    let template = DocxTemplate::open(&variant).expect("open the path template");

    let first = template
        .render(&json!({"name": "First"}), &RenderOptions::compat())
        .expect("render the original source")
        .to_bytes()
        .expect("serialize the original output");
    assert!(String::from_utf8(zip_part(&first, "word/document.xml"))
        .expect("original document XML is UTF-8")
        .contains("First"));

    rewrite_document(&source, &variant, "{{name}}", "{{other}}");
    let changed = template
        .render(&json!({"other": "Changed"}), &RenderOptions::compat())
        .expect("render the changed source")
        .to_bytes()
        .expect("serialize the changed output");
    let changed_xml = String::from_utf8(zip_part(&changed, "word/document.xml"))
        .expect("changed document XML is UTF-8");
    assert!(changed_xml.contains("Changed"));
    assert!(!changed_xml.contains("{{other}}"));
}

#[test]
fn zipname_has_priority_over_embedded_crc() {
    let tpl = template("p7_embedded_zipname");
    let zip_bytes = b"zipname-wins";
    let mut session = tpl
        .render_session(&RenderOptions::compat())
        .expect("create the session");
    session
        .replace_embedded(media("p7_embed_orig1.bin"), b"crc-loses")
        .replace_zipname("word/embeddings/p7_ole1.bin", zip_bytes);
    let rendered = session
        .finish(&RenderContext::new())
        .expect("finish the replacements")
        .to_bytes()
        .expect("write out the package");
    let pkg = Package::from_reader(Cursor::new(rendered), &PackageLimits::default())
        .expect("reopen the output package");
    assert_eq!(
        pkg.part("word/embeddings/p7_ole1.bin")
            .expect("the embedded part")
            .bytes()
            .unwrap(),
        zip_bytes
    );
}

#[test]
fn unmatched_byte_replacements_are_silent() {
    let tpl = template("p7_media_body");
    let mut session = tpl
        .render_session(&RenderOptions::compat())
        .expect("create the session");
    session
        .replace_media(b"missing-media", b"new")
        .replace_embedded(b"missing-embedded", b"new")
        .replace_zipname("word/missing.bin", b"new");
    session
        .finish(&RenderContext::new())
        .expect("the three unmatched byte replacement kinds must be silent");
}

#[test]
fn replace_pic_matches_description() {
    let workspace_target = root().join("target");
    let dir = tempfile::Builder::new()
        .prefix("p7-descr-")
        .tempdir_in(workspace_target)
        .expect("create a tempdir in the project target");
    let src = root().join("tests/fixtures/templates/p7_pic_match.docx");
    let variant = dir.path().join("template.docx");
    rewrite_document(
        &src,
        &variant,
        r#"title="my-title""#,
        r#"descr="my-description""#,
    );
    let tpl = DocxTemplate::open(&variant).expect("open the descr variant");
    let mut session = tpl
        .render_session(&RenderOptions::compat())
        .expect("create the session");
    session.replace_pic("my-description", media("p7_new4x4.png"));
    session
        .finish(&RenderContext::new())
        .expect("descr should match the picture");
}

#[test]
fn first_registered_pic_key_shadows_later_key_on_same_picture() {
    let tpl = template("p7_pic_match");
    let mut session = tpl
        .render_session(&RenderOptions::compat())
        .expect("create the session");
    session
        .replace_pic("titled-2x2.png", media("p7_media3x2.png"))
        .replace_pic("my-title", media("p7_new4x4.png"));
    let err = match session.finish(&RenderContext::new()) {
        Ok(_) => panic!("the later-registered title is shadowed by the same picture's name and should stay unmatched"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("Picture my-title not found"));
}

#[test]
fn duplicate_anchor_url_reuses_relationship_id() {
    let template = root().join("tests/fixtures/templates/p4_rt_url.docx");
    let tpl = DocxTemplate::open(template).expect("open the template");
    let mut session = tpl
        .render_session(&RenderOptions::compat())
        .expect("create the render session");
    let first = session.build_url_id("https://example.com/a?x=1&y=2");
    let second = session.build_url_id("https://example.com/a?x=1&y=2");
    assert_eq!(first, second);
}
