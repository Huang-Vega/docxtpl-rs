//! docxtpl-xml: an editable XML fragment layer (zero runtime dependencies).
//!
//! Two parsing strategies are provided (see ADR-002, ADR-004):
//! - [`XmlDocument::parse_strict`]: a standard well-formed XML subset with
//!   no recovery, used to validate the raw parts produced by python-docx;
//! - [`XmlDocument::parse_lenient`]: emulates the healing rules of libxml2
//!   `recover` on the rendering corpus, used for XML coming out of the
//!   string rendering pipeline; each healing action records a
//!   [`Recovery`] diagnostic. Security errors such as excessive depth or
//!   DTD/external entities are reported in both modes.
//!
//! The tree is a Vec arena (nodes store a parent index); elements keep
//! attributes in input order, their lexical prefix, and the xmlns
//! declarations carried on the opening tag; consecutive character data is
//! merged into a single text node. Serialization follows lxml style
//! (self-closed empty elements, double-quoted attributes, always escaping
//! `>`). It does not aim for byte-identical output to lxml (differences are
//! normalized away by c14n), but the tree structure matches.

mod error;
mod input;
mod lenient;
mod model;
mod names;
mod serialize;
mod strict;

pub use error::{XmlError, XmlOutputLimitError};
pub use model::ns_uri;
pub use model::{NodeId, NodeKind, QName, XmlDocument, XmlLimits};

/// Lenient parse result: the healed document and all recovery diagnostics.
#[derive(Debug)]
pub struct ParseOutcome {
    /// Parsed document tree.
    pub doc: XmlDocument,
    /// All recovery actions that occurred during parsing (in occurrence order).
    pub diagnostics: Vec<Recovery>,
}

/// Diagnostic information for a single recovery action.
#[derive(Clone, Debug)]
pub struct Recovery {
    /// Recovery category.
    pub kind: RecoveryKind,
    /// Byte offset of the trigger position (from the start of the input).
    pub offset: usize,
    /// 1-based line number.
    pub line: usize,
    /// 1-based column number.
    pub col: usize,
    /// Human-readable detail.
    pub detail: String,
}

/// Recovery action categories, one-to-one with the empirically probed rules
/// of libxml2 recover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryKind {
    /// An invalid entity reference was dropped (bare `&`, undefined entity,
    /// malformed numeric reference).
    BadEntityDropped,
    /// An unclosed element was auto-closed (EOF, ancestor closed first,
    /// `<` encountered before the tag closed).
    TagAutoClosed,
    /// An end tag not on the ancestor stack was treated as closing the
    /// top-of-stack element.
    StrayEndTag,
    /// Malformed markup (stray `<`, malformed attribute, unclosed
    /// comment/PI, etc.).
    MalformedTag,
    /// Stray content before or after the root element was dropped.
    PrologTailDropped,
    /// Other recovery not covered by the categories above.
    Other,
}

impl XmlDocument {
    /// Strict parsing: the input must be well-formed; any
    /// syntax/entity/security problem returns [`XmlError`].
    pub fn parse_strict(xml: &str, limits: &XmlLimits) -> Result<XmlDocument, XmlError> {
        strict::parse(xml, limits)
    }

    /// Lenient parsing: structural damage is healed per the libxml2 recover
    /// rules; security errors such as excessive depth or DTD/external
    /// entities still return [`XmlError`].
    pub fn parse_lenient(xml: &str, limits: &XmlLimits) -> Result<ParseOutcome, XmlError> {
        lenient::parse(xml, limits)
    }

    /// Serialize the whole document in lxml style, pruning xmlns
    /// declarations on elements that duplicate an ancestor's, matching
    /// lxml behavior after the body is re-attached.
    #[must_use]
    pub fn serialize(&self) -> String {
        serialize::serialize(self)
    }

    /// Serialize the full document while enforcing an allocation-time byte
    /// budget.  Unlike checking [`String::len`] afterwards, this stops before
    /// XML entity expansion can allocate an oversized result.
    pub fn try_serialize(&self, max_bytes: usize) -> Result<String, XmlOutputLimitError> {
        serialize::serialize_with_limit(
            self,
            serialize::SerializeOptions {
                retain_redundant_ns: false,
            },
            max_bytes,
        )
    }

    /// Header/footer-specific serialization: retains the xmlns declarations
    /// carried lexically on element opening tags even when they duplicate an
    /// ancestor's, matching the lxml output of a story part parsed
    /// independently via `XmlPart.load` with no re-attachment (ADR-006).
    #[must_use]
    pub fn serialize_story(&self) -> String {
        serialize::serialize_with(
            self,
            serialize::SerializeOptions {
                retain_redundant_ns: true,
            },
        )
    }

    /// Story-part variant of [`Self::try_serialize`], retaining redundant
    /// namespace declarations to match the unbounded story serializer.
    pub fn try_serialize_story(&self, max_bytes: usize) -> Result<String, XmlOutputLimitError> {
        serialize::serialize_with_limit(
            self,
            serialize::SerializeOptions {
                retain_redundant_ns: true,
            },
            max_bytes,
        )
    }

    /// Serialize the subtree rooted at `id` (without an XML declaration).
    ///
    /// Used for Subdoc fragment output (ADR-007): upstream does `tostring`
    /// on the sub body and then strips the body tag, so fragments carry no
    /// namespace declarations.
    #[must_use]
    pub fn serialize_subtree(&self, id: NodeId) -> String {
        serialize::serialize_subtree(
            self,
            id,
            serialize::SerializeOptions {
                retain_redundant_ns: false,
            },
        )
    }

    /// Bounded variant of [`Self::serialize_subtree`].
    pub fn try_serialize_subtree(
        &self,
        id: NodeId,
        max_bytes: usize,
    ) -> Result<String, XmlOutputLimitError> {
        serialize::serialize_subtree_with_limit(
            self,
            id,
            serialize::SerializeOptions {
                retain_redundant_ns: false,
            },
            max_bytes,
        )
    }

    /// Remove text nodes that consist solely of XML whitespace and are not
    /// within the scope of `xml:space="preserve"`.
    ///
    /// Matches the python-docx oxml parser's `remove_blank_text=True`:
    /// headers/footers mapped to new `XmlPart`s are parsed with that option,
    /// so the newline/indentation whitespace that python-docx templates
    /// carry in injected image XML is stripped (ADR-006); but when elements
    /// such as `w:t` explicitly mark `xml:space="preserve"`, their
    /// whitespace text must be retained (libxml2 semantics for this parse
    /// option: xml:space is tracked along the ancestor axis, `preserve`
    /// keeps text, `default` restores pruning).
    pub fn strip_blank_text(&mut self) {
        let root = self.root();
        let mut blank = Vec::new();
        let mut stack: Vec<(NodeId, bool)> = vec![(root, false)];
        while let Some((id, preserved)) = stack.pop() {
            let mut preserved = preserved;
            if self.node_kind(id) == NodeKind::Element {
                if let Some(mode) = self.attr(id, ns_uri::XML, "space") {
                    preserved = mode == "preserve";
                }
            } else if self.node_kind(id) == NodeKind::Text
                && !preserved
                && !self.node_value(id).is_empty()
                && self
                    .node_value(id)
                    .chars()
                    .all(|c| matches!(c, ' ' | '\t' | '\r' | '\n'))
            {
                blank.push(id);
            }
            // The stack is LIFO; reverse child order to process in document
            // order (order does not affect the result, this only eases debugging).
            for child in self.children(id).iter().rev() {
                stack.push((*child, preserved));
            }
        }
        for id in blank {
            self.detach(id);
        }
    }
}
