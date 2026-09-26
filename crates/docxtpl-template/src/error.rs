//! Rendering error types (code spec §3.2: layered, locatable, context-preserving).

use docxtpl_xml::XmlError;
use std::fmt;

/// Stable categories of template errors (the exact messages may evolve; the
/// category is used for oracle error-classification compatibility).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateErrorKind {
    /// Template syntax error (aligned with jinja2 `TemplateSyntaxError`).
    Syntax,
    /// Undefined variable/attribute (under strict undefined).
    Undefined,
    /// Unrecognized image bytes (aligned with docxtpl `UnrecognizedImageError`;
    /// triggered when InlineImage probe/size conversion fails).
    Image,
    /// Invalid argument value: includes a docxtpl `replace_pic` miss, as well as
    /// the error paths where compat filters/methods mirror Python `ValueError`.
    InvalidArgument,
    /// Other template execution errors.
    Other,
}

impl TemplateErrorKind {
    /// The Python exception class name used in the oracle report.
    #[must_use]
    pub fn oracle_exception(self) -> &'static str {
        match self {
            TemplateErrorKind::Syntax => "TemplateSyntaxError",
            TemplateErrorKind::Undefined => "UndefinedError",
            TemplateErrorKind::Image => "UnrecognizedImageError",
            TemplateErrorKind::InvalidArgument => "ValueError",
            TemplateErrorKind::Other => "TemplateError",
        }
    }
}

/// Rendering pipeline errors.
#[derive(Debug)]
pub enum RenderError {
    /// Rendering exceeded the resource budget.
    Limit {
        /// Name of the failing part.
        part: String,
        /// Limit category.
        kind: &'static str,
        /// Maximum allowed value.
        max: u64,
    },
    /// XML parsing/healing/post-processing failed.
    Xml {
        /// Name of the failing part.
        part: String,
        /// Underlying XML error.
        source: XmlError,
    },

    /// Template syntax or evaluation error.
    Template {
        /// Stable category.
        kind: TemplateErrorKind,
        /// Name of the failing part.
        part: String,
        /// 1-based line number reported by MiniJinja, if any.
        line: Option<usize>,
        /// Error message.
        message: String,
        /// Mirrors upstream `exc.docx_context`: tag-stripped plain-text
        /// snippets around the failing line.
        context: Vec<String>,
    },
}

impl RenderError {
    /// Returns the stable error category (`None` for XML errors).
    #[must_use]
    pub fn kind(&self) -> Option<TemplateErrorKind> {
        match self {
            RenderError::Template { kind, .. } => Some(*kind),
            RenderError::Xml { .. } | RenderError::Limit { .. } => None,
        }
    }

    /// Text context around the error (mirrors upstream docx_context).
    #[must_use]
    pub fn context_lines(&self) -> &[String] {
        match self {
            RenderError::Template { context, .. } => context,
            RenderError::Xml { .. } | RenderError::Limit { .. } => &[],
        }
    }

    /// Name of the failing part.
    #[must_use]
    pub fn part(&self) -> &str {
        match self {
            RenderError::Xml { part, .. }
            | RenderError::Template { part, .. }
            | RenderError::Limit { part, .. } => part,
        }
    }
}

impl std::error::Error for RenderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RenderError::Xml { source, .. } => Some(source),
            RenderError::Template { .. } | RenderError::Limit { .. } => None,
        }
    }
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenderError::Limit { part, kind, max } => {
                write!(
                    f,
                    "rendering resource limit exceeded (part {part}, {kind} max {max})"
                )
            }
            RenderError::Xml { part, source } => {
                write!(f, "XML processing failed (part {part}): {source}")
            }
            RenderError::Template {
                kind,
                part,
                line,
                message,
                ..
            } => {
                let line = line
                    .map(|l| l.to_string())
                    .unwrap_or_else(|| "?".to_string());
                write!(
                    f,
                    "template error (part {part}, line {line}): {kind:?}: {message}"
                )
            }
        }
    }
}
