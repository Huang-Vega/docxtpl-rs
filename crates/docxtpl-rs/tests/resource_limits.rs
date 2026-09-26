use std::path::PathBuf;

use docxtpl_rs::{DocxTemplate, Error, RenderOptions, ResourceLimits};
use serde_json::json;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/templates")
        .join(name)
}

#[test]
fn large_document_defaults_are_public_and_consistent() {
    let limits = ResourceLimits::default();
    assert_eq!(limits.max_input_docx_bytes(), 600 * 1024 * 1024);
    assert_eq!(limits.max_rendered_xml_bytes(), 600 * 1024 * 1024);
    assert_eq!(limits.package_limits().max_entries, 6_000);
    assert_eq!(
        limits.package_limits().max_entry_uncompressed,
        600 * 1024 * 1024
    );
    assert_eq!(
        limits.package_limits().max_total_uncompressed,
        600 * 1024 * 1024
    );
    assert_eq!(limits.package_limits().max_output_size, 600 * 1024 * 1024);
}

#[test]
fn file_input_limit_is_checked_before_opening_the_package() {
    let error = DocxTemplate::open_with_limits(
        fixture("r2_var_basic.docx"),
        ResourceLimits::default().with_max_input_docx_bytes(1),
    )
    .expect_err("fixture must exceed one byte");
    assert!(matches!(error, Error::InputTooLarge { max: 1 }));
}

#[test]
fn configured_render_limit_reaches_the_render_pipeline() {
    let template = DocxTemplate::open_with_limits(
        fixture("r2_var_basic.docx"),
        ResourceLimits::default().with_max_rendered_xml_bytes(16),
    )
    .expect("open fixture");
    let error = template
        .render(&json!({"name": "Vega"}), &RenderOptions::compat())
        .expect_err("serialized document XML must exceed sixteen bytes");
    match error {
        Error::Render(error) => {
            let message = error.to_string();
            assert!(message.contains("rendered_xml_bytes"), "{message}");
            assert!(message.contains("16"), "{message}");
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn file_templates_do_not_retain_a_second_compressed_copy() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("template.docx");
    std::fs::copy(fixture("r2_var_basic.docx"), &source).expect("copy fixture");
    let template = DocxTemplate::open(&source).expect("open fixture");
    std::fs::remove_file(&source).expect("remove source after validation");

    let error = template
        .render(&json!({"name": "Vega"}), &RenderOptions::compat())
        .expect_err("path-backed templates reopen their source");
    assert!(matches!(error, Error::Io(_)));
}
