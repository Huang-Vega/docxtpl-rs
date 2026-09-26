//! DEV-0008：Subdoc 仅提供外部 DOCX 路径模式。
//!
//! 无路径的借用模式由 `RenderSession::new_subdoc` 的 compile-fail 文档
//! 测试覆盖；这里在集成测试边界锁定公开 API 的路径参数与返回类型，避免
//! 后续把方法意外改成复用当前文档 part 的借用入口。

use std::path::PathBuf;

use docxtpl_rs::{DocxTemplate, RenderOptions, RenderValue};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn new_subdoc_path_mode_returns_a_typed_subdoc_value() {
    let fixture_root = root().join("tests/fixtures/templates");
    let template =
        DocxTemplate::open(fixture_root.join("p6_subdoc_basic.docx")).expect("打开主模板");
    let mut session = template
        .render_session(&RenderOptions::compat())
        .expect("创建渲染会话");

    let value = session
        .new_subdoc(fixture_root.join("p6_subdoc_basic_sub.docx"))
        .expect("外部路径模式应成功");

    assert!(matches!(value, RenderValue::Subdoc(_)));
}
