//! docxtpl-template：对齐 docxtpl 0.20.2 的模板渲染管线。
//!
//! 管线（与上游 `render_xml_part` / `fix_tables` / `fix_docpr_ids` 同构，
//! 见 ADR-002 与 docs/compatibility.md §2）：
//!
//! 1. [`docxtpl_compat::patch_xml`]：13 步有界正则变换；
//! 2. 每个 `<w:p>` 前插 `\n`（仅为错误行号对齐）；
//! 3. MiniJinja 渲染（默认 autoescape=false、lenient undefined，见 ADR-003）；
//! 4. 移除步骤 2 的换行、还原 `{_{ }_}` 字面转义；
//! 5. [`docxtpl_compat::resolve_listing`]：`\n \t \a \f` 映射；
//! 6. docxtpl-xml 宽松(recover)解析愈合（诊断随结果返回）；
//! 7. [`fix_tables::fix_tables`] 与 [`fix_tables::fix_docpr_ids`]；
//! 8. lxml 风格序列化。

mod core_props;
mod error;
mod fix_tables;
mod render;

pub use core_props::render_core_properties;
pub use error::{RenderError, TemplateErrorKind};
pub use render::{render_document_xml, RenderOptions, RenderOutcome};
