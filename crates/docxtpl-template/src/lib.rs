//! docxtpl-template：对齐 docxtpl 0.20.2 的模板渲染管线。
//!
//! 管线（与上游 `render_xml_part` / `fix_tables` / `fix_docpr_ids` 同构，
//! 见 ADR-002/006 与 docs/compatibility.md §2）：
//!
//! 1. [`docxtpl_compat::patch_xml`]：13 步有界正则变换；
//! 2. 每个 `<w:p>` 前插 `\n`（仅为错误行号对齐）；
//! 3. MiniJinja 渲染（默认 autoescape=false、lenient undefined，见 ADR-003）；
//! 4. 移除步骤 2 的换行、还原 `{_{ }_}` 字面转义；
//! 5. [`docxtpl_compat::resolve_listing`]：`\n \t \a \f` 映射；
//! 6. docxtpl-xml 宽松(recover)解析愈合（诊断随结果返回）；
//! 7. [`fix_tables::fix_tables`] 与 [`fix_tables::fix_docpr_ids`]（仅正文，
//!    P5 页眉页脚 story part 跳过，见 ADR-006）；
//! 8. lxml 风格序列化。
//!
//! Part 入口（P5，ADR-006）：[`render_document_xml_ctx`]（正文）、
//! [`render_story_xml_ctx`]（页眉/页脚，无 fix_*）、
//! [`render_footnotes_xml_ctx`]（脚注，原始字符串输出不重序列化）。

mod context;
mod core_props;
mod error;
mod fix_tables;
mod render;

pub use context::{
    ImageRegistry, ImageRels, ImageResolveError, NullRegistry, RenderContext, RenderValue,
    SubdocFragment,
};
pub use core_props::{render_core_properties, render_core_properties_ctx};
pub use error::{RenderError, TemplateErrorKind};
pub use render::{
    find_undeclared_variables, normalize_part_xml, render_document_xml, render_document_xml_ctx,
    render_footnotes_xml_ctx, render_story_xml_ctx, shape_id_of, RenderOptions, RenderOutcome,
};
