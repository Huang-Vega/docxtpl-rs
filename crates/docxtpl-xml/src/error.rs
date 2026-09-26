//! XML parsing error types.

/// All errors related to XML parsing and security policy.
///
/// Expected failures are always returned via [`Result`] (see ADR-004: library
/// code must not use unwrap/expect/panic). Lenient (recover) parsing only
/// heals structural damage; security errors (excessive depth,
/// DTD/external entities) are returned in both modes.
#[derive(Debug, thiserror::Error)]
pub enum XmlError {
    /// Generic syntax/well-formedness error, with 1-based line/column numbers
    /// and a byte offset.
    #[error("XML parse failed (line {line}, column {col}): {message}")]
    Parse {
        /// 1-based line number (counting `\n`).
        line: usize,
        /// 1-based column number (counting Unicode scalars).
        col: usize,
        /// Byte offset from the start of the input.
        offset: usize,
        /// Human-readable error detail.
        message: String,
    },
    /// Element nesting depth exceeded the limit (default in [`crate::XmlLimits`]).
    #[error("XML nesting exceeds {0} levels")]
    NestingLimitExceeded(usize),
    /// Encountered a forbidden DTD or external/general entity declaration.
    #[error("DTD/external entities forbidden: {0}")]
    EntityForbidden(String),
    /// Incomplete input: premature end of input, missing root element, etc.
    #[error("XML incomplete: {0}")]
    Incomplete(String),
}

/// A serialized XML result would exceed the caller-provided byte budget.
///
/// This is separate from [`XmlError`] because the XML tree itself is valid;
/// only the requested output allocation is too large.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("XML serialized output exceeds {max} bytes")]
pub struct XmlOutputLimitError {
    /// Maximum serialized byte length requested by the caller.
    pub max: usize,
}

impl XmlError {
    /// Construct a syntax error at the given byte offset.
    pub(crate) fn at(input: &str, offset: usize, message: impl Into<String>) -> Self {
        let (line, col) = crate::input::line_col(input, offset);
        Self::Parse {
            line,
            col,
            offset,
            message: message.into(),
        }
    }
}
