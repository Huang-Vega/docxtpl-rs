//! Port of upstream `DocxTemplate.render_properties` (template.py L333–358).
//!
//! On every render, upstream renders the six string-typed core properties as
//! Jinja templates and writes them back: missing elements (e.g.
//! dc:identifier / dc:language) are therefore created as empty elements.
//! The core-properties part uses strict parsing (the original part is
//! well-formed), with no recovery.

use docxtpl_xml::XmlDocument;
use docxtpl_xml::XmlLimits;
use serde_json::Value as JsonValue;

use crate::context::{ImageRegistry, NullRegistry, RenderContext};
use crate::error::{RenderError, TemplateErrorKind};
use crate::render::{
    render_inline_value_with_limit, substitute_images_with_limit, ImageProbeOptions,
    PreparedRenderContext, RenderOptions,
};

/// Dublin Core elements namespace (dc:title, etc.).
const DC_NS: &str = "http://purl.org/dc/elements/1.1/";

/// Core-properties part name.
pub const CORE_PART: &str = "docProps/core.xml";

/// (Order in the upstream properties list, dc local name).
/// python-docx property name → OOXML element name:
/// author→creator, comments→description; the rest share the same name.
const PROPERTIES: [(&str, &str); 6] = [
    ("author", "creator"),
    ("comments", "description"),
    ("identifier", "identifier"),
    ("language", "language"),
    ("subject", "subject"),
    ("title", "title"),
];

/// Renders docProps/core.xml (aligned with upstream defaults: autoescape does
/// not apply to property rendering; when jinja_env is absent, `Environment()`).
pub fn render_core_properties(
    src_xml: &str,
    context: &JsonValue,
    autoescape: bool,
) -> Result<String, RenderError> {
    let ctx = RenderContext::try_from_json(context).map_err(|error| RenderError::Template {
        kind: TemplateErrorKind::InvalidArgument,
        part: CORE_PART.to_string(),
        line: None,
        message: error.to_string(),
        context: Vec::new(),
    })?;
    let mut null_registry = NullRegistry;
    let options = RenderOptions::compat().with_autoescape(autoescape);
    render_core_properties_ctx_with_options(src_xml, &ctx, &options, &mut null_registry)
}

/// Rich-content version (P4): core properties share the same context as the
/// main document (as in upstream `render_properties`). RichText XML is
/// injected into dc elements as-is; image values are meaningless in this part
/// but resolve idempotently (relationships still belong to the main document).
pub fn render_core_properties_ctx(
    src_xml: &str,
    context: &RenderContext,
    autoescape: bool,
    registry: &mut dyn ImageRegistry,
) -> Result<String, RenderError> {
    let options = RenderOptions::compat().with_autoescape(autoescape);
    render_core_properties_ctx_with_options(src_xml, context, &options, registry)
}

/// Renders core properties with the full [`RenderOptions`], keeping the
/// Rust-native environment configuration consistent with
/// document/story/footnotes parts.
pub fn render_core_properties_ctx_with_options(
    src_xml: &str,
    context: &RenderContext,
    options: &RenderOptions,
    registry: &mut dyn ImageRegistry,
) -> Result<String, RenderError> {
    let prepared = crate::render::prepare_render_context(context, CORE_PART)?;
    render_core_properties_prepared(src_xml, &prepared, options, registry)
}

/// Renders core properties with a context prepared once for a multi-part render.
pub fn render_core_properties_prepared(
    src_xml: &str,
    context: &PreparedRenderContext<'_>,
    options: &RenderOptions,
    registry: &mut dyn ImageRegistry,
) -> Result<String, RenderError> {
    let max = options.max_rendered_xml_bytes();
    let environment = crate::render::build_jinja_env_with_options(options);
    let mut doc = XmlDocument::parse_strict(src_xml, &XmlLimits::default()).map_err(|e| {
        RenderError::Xml {
            part: CORE_PART.to_string(),
            source: e,
        }
    })?;

    let doc_root = doc.root();
    // The six properties are rendered independently, so the per-template
    // writer limit is not an aggregate bound.  Count a conservative upper
    // bound for the whole serialized part before storing all six strings in
    // the DOM.  Starting with the source length deliberately double-counts
    // replaced source text; core.xml is small in ordinary DOCX files and the
    // conservative accounting avoids a multi-property amplification path.
    let mut serialized_budget = src_xml.len();

    for (_python_name, local) in PROPERTIES {
        let existing = doc.children(doc_root).iter().copied().find(|&c| {
            doc.tag(c)
                .is_some_and(|q| q.ns == DC_NS && q.local == local)
        });
        // python-docx defaults these text-typed properties to the empty
        // string (the getter returns '').
        let initial = existing.map_or(String::new(), |id| {
            doc.element_text(id)
                .map_or_else(String::new, str::to_string)
        });
        let rendered = render_inline_value_with_limit(
            &environment,
            &initial,
            context.root.clone(),
            CORE_PART,
            max,
        )?;
        // Pathological case where a property value references an image:
        // resolve in order of appearance (relationships belong to the current
        // registry scope, as in the main flow); normal documents never put
        // images inside dc elements.
        let rendered = substitute_images_with_limit(
            &rendered,
            registry,
            &context.pending_images,
            1,
            CORE_PART,
            max,
            ImageProbeOptions::from_render_options(options),
        )?;
        add_core_text_budget(&mut serialized_budget, &rendered, max)?;
        let element = match existing {
            Some(id) => id,
            None => {
                // Missing element: create and append to the root per the
                // upstream setattr behavior (the dc prefix must be declared).
                let new_id = doc
                    .new_prefixed_element("dc", DC_NS, local, Vec::new())
                    .map_err(|e| RenderError::Xml {
                        part: CORE_PART.to_string(),
                        source: e,
                    })?;
                doc.append_child(doc_root, new_id);
                new_id
            }
        };
        doc.set_element_text(element, &rendered);
    }

    doc.try_serialize(max).map_err(|_| core_output_limit(max))
}

fn core_output_limit(max: usize) -> RenderError {
    RenderError::Limit {
        part: CORE_PART.to_string(),
        kind: "rendered_xml_bytes",
        max: max as u64,
    }
}

fn add_core_text_budget(budget: &mut usize, value: &str, max: usize) -> Result<(), RenderError> {
    let escaped_len = value.chars().try_fold(0usize, |total, ch| {
        // XML text serialization expands these three characters; quotes do
        // not need escaping outside attributes.  All other scalar values keep
        // their UTF-8 byte length.
        let encoded = match ch {
            '&' => 5,
            '<' | '>' => 4,
            other => other.len_utf8(),
        };
        total.checked_add(encoded)
    });
    *budget = escaped_len
        .and_then(|length| budget.checked_add(length))
        .ok_or_else(|| core_output_limit(max))?;
    if *budget > max {
        return Err(core_output_limit(max));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use docxtpl_xml::XmlLimits;
    use serde_json::json;

    fn dc_text(xml_out: &str, local: &str) -> Option<String> {
        let doc = XmlDocument::parse_strict(xml_out, &XmlLimits::default()).unwrap();
        doc.children(doc.root())
            .iter()
            .copied()
            .find(|&c| {
                doc.tag(c)
                    .is_some_and(|q| q.ns == DC_NS && q.local == local)
            })
            .map(|c| doc.element_text(c).unwrap_or("").to_string())
    }

    const SAMPLE: &str = concat!(
        "<?xml version='1.0' encoding='UTF-8' standalone='yes'?>",
        "<cp:coreProperties",
        " xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\"",
        " xmlns:dc=\"http://purl.org/dc/elements/1.1/\">",
        "<dc:creator>python-docx</dc:creator>",
        "<dc:description>generated by python-docx</dc:description>",
        "<dc:title></dc:title><cp:category/>",
        "</cp:coreProperties>"
    );

    #[test]
    fn creates_missing_properties_and_keeps_existing() {
        let out = render_core_properties(SAMPLE, &json!({}), false).unwrap();
        // Existing values are kept verbatim (jinja identity rendering).
        assert_eq!(dc_text(&out, "creator").as_deref(), Some("python-docx"));
        assert_eq!(
            dc_text(&out, "description").as_deref(),
            Some("generated by python-docx")
        );
        // Upstream setattr side effect: missing identifier/language are
        // filled in as empty elements.
        assert_eq!(dc_text(&out, "identifier").as_deref(), Some(""));
        assert_eq!(dc_text(&out, "language").as_deref(), Some(""));
        assert_eq!(dc_text(&out, "title").as_deref(), Some(""));
        // New elements are appended at the end of the root, with identifier
        // before language (properties list order).
        let i = out.find("<dc:identifier").unwrap();
        let l = out.find("<dc:language").unwrap();
        assert!(i < l);
    }

    #[test]
    fn renders_jinja_in_property_values() {
        let xml = SAMPLE.replace("<dc:title></dc:title>", "<dc:title>{{ x }}</dc:title>");
        let out = render_core_properties(&xml, &json!({"x": "hello"}), false).unwrap();
        assert_eq!(dc_text(&out, "title").as_deref(), Some("hello"));
    }

    #[test]
    fn aggregate_core_budget_counts_xml_expansion() {
        let max = crate::render::MAX_RENDERED_XML_BYTES;
        let mut budget = max - 5;
        add_core_text_budget(&mut budget, "&", max).unwrap();
        assert_eq!(budget, max);
        assert!(matches!(
            add_core_text_budget(&mut budget, "x", max),
            Err(RenderError::Limit {
                part,
                kind: "rendered_xml_bytes",
                ..
            }) if part == CORE_PART
        ));
    }
}
