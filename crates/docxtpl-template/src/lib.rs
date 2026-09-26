//! docxtpl-template: the template rendering pipeline aligned with docxtpl 0.20.2.
//!
//! The pipeline (isomorphic to upstream `render_xml_part` / `fix_tables` /
//! `fix_docpr_ids`; see ADR-002/006 and docs/compatibility.md §2):
//!
//! 1. [`docxtpl_compat::patch_xml`]: 13 bounded regex transformations;
//! 2. Insert `\n` before every `<w:p>` (only for error line-number alignment);
//! 3. MiniJinja rendering (defaults: autoescape=false, lenient undefined; see ADR-003);
//! 4. Remove the newlines from step 2 and restore `{_{ }_}` literal escapes;
//! 5. [`docxtpl_compat::resolve_listing`]: the `\n \t \a \f` mapping;
//! 6. Lenient (recover) parsing/healing via docxtpl-xml (diagnostics returned with the result);
//! 7. Internal `fix_tables` and `fix_docpr_ids` (body only;
//!    P5 header/footer story parts are skipped; see ADR-006);
//! 8. lxml-style serialization.
//!
//! Part entry points (P5, ADR-006): [`render_document_xml_ctx`] (body),
//! [`render_story_xml_ctx`] (header/footer, no fix_*),
//! [`render_footnotes_xml_ctx`] (footnotes; raw string output, no re-serialization).

mod context;
mod core_props;
mod error;
mod fix_tables;
mod render;

pub use context::{
    ImageRegistry, ImageRels, ImageResolveError, JsonContextError, NullRegistry, RenderContext,
    RenderValue, SubdocFragment,
};
pub use core_props::{
    render_core_properties, render_core_properties_ctx, render_core_properties_ctx_with_options,
};
pub use error::{RenderError, TemplateErrorKind};
pub use render::{
    find_undeclared_variables, normalize_part_xml, render_document_xml, render_document_xml_ctx,
    render_footnotes_xml_ctx, render_story_xml_ctx, shape_id_of, EnvironmentConfigurator,
    RenderOptions, RenderOutcome,
};

/// MiniJinja is re-exported so callers configuring [`RenderOptions`] do not
/// need a second direct dependency just to name filter/test/function types.
pub use minijinja;
