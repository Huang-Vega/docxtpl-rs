//! DEV-0008: Subdoc only offers the external DOCX path mode.
//!
//! The borrowed mode without a path is covered by the compile-fail doc test
//! of `RenderSession::new_subdoc`; here the integration-test boundary pins
//! the public API path parameter and return type, so that the method is not
//! accidentally changed into a borrowed entry point reusing parts of the
//! current document.

use std::path::PathBuf;

use docxtpl_rs::{DocxTemplate, RenderOptions, RenderValue};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn new_subdoc_path_mode_returns_a_typed_subdoc_value() {
    let fixture_root = root().join("tests/fixtures/templates");
    let template = DocxTemplate::open(fixture_root.join("p6_subdoc_basic.docx"))
        .expect("open the main template");
    let mut session = template
        .render_session(&RenderOptions::compat())
        .expect("create the render session");

    let value = session
        .new_subdoc(fixture_root.join("p6_subdoc_basic_sub.docx"))
        .expect("external path mode should succeed");

    assert!(matches!(value, RenderValue::Subdoc(_)));
}
