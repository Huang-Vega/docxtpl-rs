//! Retentive serializer whose output style matches lxml `tostring`
//! byte-for-byte (the oracle diff compares both the raw part bytes and
//! c14n, so details like declaration quoting/newlines must also match).

use std::convert::Infallible;

use crate::error::XmlOutputLimitError;
use crate::model::ns_uri;
use crate::model::{NodeId, NodeKind, XmlDocument};

trait XmlOutput {
    type Error;

    fn push_str(&mut self, value: &str) -> Result<(), Self::Error>;

    fn push(&mut self, value: char) -> Result<(), Self::Error> {
        let mut encoded = [0; 4];
        self.push_str(value.encode_utf8(&mut encoded))
    }
}

struct UnboundedOutput(String);

impl XmlOutput for UnboundedOutput {
    type Error = Infallible;

    fn push_str(&mut self, value: &str) -> Result<(), Self::Error> {
        self.0.push_str(value);
        Ok(())
    }
}

struct BoundedOutput {
    value: String,
    max: usize,
}

impl XmlOutput for BoundedOutput {
    type Error = XmlOutputLimitError;

    fn push_str(&mut self, value: &str) -> Result<(), Self::Error> {
        if value.len() > self.max.saturating_sub(self.value.len()) {
            return Err(XmlOutputLimitError { max: self.max });
        }
        self.value.push_str(value);
        Ok(())
    }
}

/// Serialization toggles.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SerializeOptions {
    /// Whether to retain xmlns declarations carried lexically on an
    /// element's opening tag but duplicated by an ancestor.
    ///
    /// When rendering the body, lxml re-attaches the rendered tree under
    /// the original document element, and serialization strips such
    /// redundant declarations (the default pruning behavior, `false`);
    /// headers/footers are new trees parsed independently via
    /// `XmlPart.load` (no re-attachment), so redundant declarations such
    /// as `xmlns:wp/xmlns:r` on an injected image `wp:inline` must be
    /// retained verbatim (ADR-006).
    pub retain_redundant_ns: bool,
}

/// Serialize the whole document; when an XML declaration is present, emit
/// the lxml-style single-quoted declaration followed by a newline (matching
/// the document.xml written by python-docx).
pub(crate) fn serialize(doc: &XmlDocument) -> String {
    serialize_with(
        doc,
        SerializeOptions {
            retain_redundant_ns: false,
        },
    )
}

/// Serialize with options (used for headers/footers, see
/// [`SerializeOptions`]).
pub(crate) fn serialize_with(doc: &XmlDocument, options: SerializeOptions) -> String {
    let mut out = UnboundedOutput(String::new());
    let result = serialize_document(doc, options, &mut out);
    match result {
        Ok(()) => out.0,
        Err(never) => match never {},
    }
}

/// Allocation-time bounded document serialization.
pub(crate) fn serialize_with_limit(
    doc: &XmlDocument,
    options: SerializeOptions,
    max_bytes: usize,
) -> Result<String, XmlOutputLimitError> {
    let mut out = BoundedOutput {
        value: String::new(),
        max: max_bytes,
    };
    serialize_document(doc, options, &mut out)?;
    Ok(out.value)
}

fn serialize_document<O: XmlOutput>(
    doc: &XmlDocument,
    options: SerializeOptions,
    out: &mut O,
) -> Result<(), O::Error> {
    if doc.has_decl {
        out.push_str("<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n")?;
    }
    serialize_node(doc, doc.root(), options, out)
}

/// Serialize the subtree rooted at `id` (without an XML declaration).
///
/// Used for Subdoc fragment output: upstream runs
/// `etree.tostring(body, encoding="unicode")` and then strips the body
/// opening/closing tags with a regex, which is equivalent to serializing
/// the body's direct children one by one — fragments carry no namespace
/// declarations (the promoted declarations on the body opening tag are
/// stripped with the tag), and prefix bindings are provided by the main
/// document.
pub(crate) fn serialize_subtree(
    doc: &XmlDocument,
    id: NodeId,
    options: SerializeOptions,
) -> String {
    let mut out = UnboundedOutput(String::new());
    let result = serialize_node(doc, id, options, &mut out);
    match result {
        Ok(()) => out.0,
        Err(never) => match never {},
    }
}

/// Allocation-time bounded subtree serialization.
pub(crate) fn serialize_subtree_with_limit(
    doc: &XmlDocument,
    id: NodeId,
    options: SerializeOptions,
    max_bytes: usize,
) -> Result<String, XmlOutputLimitError> {
    let mut out = BoundedOutput {
        value: String::new(),
        max: max_bytes,
    };
    serialize_node(doc, id, options, &mut out)?;
    Ok(out.value)
}

fn serialize_node<O: XmlOutput>(
    doc: &XmlDocument,
    id: NodeId,
    options: SerializeOptions,
    out: &mut O,
) -> Result<(), O::Error> {
    let node = &doc.nodes[id.0 as usize];
    match node.kind {
        NodeKind::Text => {
            escape_text(&node.value, out)?;
        }
        NodeKind::Comment => {
            out.push_str("<!--")?;
            out.push_str(&node.value)?;
            out.push_str("-->")?;
        }
        NodeKind::Pi => {
            out.push_str("<?")?;
            out.push_str(&node.pi_target)?;
            if !node.value.is_empty() {
                out.push(' ')?;
                out.push_str(&node.value)?;
            }
            out.push_str("?>")?;
        }
        NodeKind::Element => serialize_element(doc, id, options, out)?,
    }
    Ok(())
}

fn serialize_element<O: XmlOutput>(
    doc: &XmlDocument,
    id: NodeId,
    options: SerializeOptions,
    out: &mut O,
) -> Result<(), O::Error> {
    let node = &doc.nodes[id.0 as usize];
    out.push('<')?;
    let lexical_prefix = &node.prefix;
    let local = node
        .qname
        .as_ref()
        .map(|q| q.local.as_str())
        .unwrap_or_default();
    // The body path emulates lxml cross-tree re-attachment: if the
    // element-name URI already has a binding on the ancestor axis, use the
    // ancestor prefix uniformly (the local redundant declaration is
    // pruned; see the nsdecls handling below).
    let eff_prefix = if options.retain_redundant_ns {
        lexical_prefix.clone()
    } else {
        node.qname
            .as_ref()
            .filter(|q| !q.ns.is_empty())
            .and_then(|q| ancestor_uri_prefix(doc, node.parent, &q.ns))
            .unwrap_or_else(|| lexical_prefix.clone())
    };
    if eff_prefix.is_empty() {
        out.push_str(local)?;
    } else {
        out.push_str(&eff_prefix)?;
        out.push(':')?;
        out.push_str(local)?;
    }
    for (prefix, uri) in &node.nsdecls {
        // The body path (retain=false) emulates lxml cross-tree
        // re-attachment: when the ancestor axis already binds the same URI
        // (under any prefix), this element drops the declaration and
        // subtree references are rebound to the ancestor prefix (P7b B4: in
        // a real Word template a local `xmlns:wp14` on a paragraph and the
        // root `xmlns:w14` share a URI, and output keeps only the w14
        // form); headers/footers are whole-tree parse/tostring (no
        // re-attachment), so everything lexical is retained (ADR-006).
        if !options.retain_redundant_ns && ancestor_uri_prefix(doc, node.parent, uri).is_some() {
            continue;
        }
        out.push(' ')?;
        if prefix.is_empty() {
            out.push_str("xmlns=\"")?;
        } else {
            out.push_str("xmlns:")?;
            out.push_str(prefix)?;
            out.push_str("=\"")?;
        }
        escape_attr(uri, out)?;
        out.push('"')?;
    }
    for (i, (qname, value)) in node.attrs.iter().enumerate() {
        out.push(' ')?;
        // The implicit xml prefix; every other prefix uses the lexical
        // prefix recorded at parse time, and the body path then rebinds it
        // to an ancestor prefix with the same URI per cross-tree
        // re-attachment rules.
        let stored = node.attr_prefix.get(i).cloned().unwrap_or_default();
        let attr_prefix = if options.retain_redundant_ns {
            stored
        } else if !stored.is_empty() && !qname.ns.is_empty() {
            ancestor_uri_prefix(doc, node.parent, &qname.ns).unwrap_or(stored)
        } else {
            stored
        };
        if !attr_prefix.is_empty() {
            out.push_str(&attr_prefix)?;
            out.push(':')?;
        } else if qname.ns == ns_uri::XML {
            out.push_str("xml:")?;
        }
        out.push_str(&qname.local)?;
        out.push_str("=\"")?;
        escape_attr(value, out)?;
        out.push('"')?;
    }
    if node.children.is_empty() {
        out.push('/')?;
        out.push('>')?;
        return Ok(());
    }
    out.push('>')?;
    // Copy the child sequence first to avoid recursive borrow conflicts.
    let children = node.children.clone();
    for child in children {
        serialize_node(doc, child, options, out)?;
    }
    out.push_str("</")?;
    if !eff_prefix.is_empty() {
        out.push_str(&eff_prefix)?;
        out.push(':')?;
    }
    out.push_str(local)?;
    out.push('>')?;
    Ok(())
}

/// Find the **effective** prefix bound to a namespace URI on `start` and
/// its ancestor axis (nearest first) (P7b B4).
///
/// Emulates namespace merging during lxml cross-tree re-attachment (the
/// rendered fragment is `parse_xml`'d and then appended into the original
/// document tree): if a local `xmlns:p="U"` on an element has a binding
/// for the same URI already on its ancestor axis, that declaration is
/// dropped and the prefix is rebound to the ancestor's; shadowing by an
/// inner declaration with the same prefix but a different URI still takes
/// precedence over outer bindings and is handled naturally by recursion.
fn ancestor_uri_prefix(doc: &XmlDocument, start: Option<NodeId>, uri: &str) -> Option<String> {
    let mut current = start;
    while let Some(id) = current {
        let node = &doc.nodes[id.0 as usize];
        if let Some((prefix, _)) = node.nsdecls.iter().find(|(_, u)| u == uri) {
            // This element declares the same URI: if the next ancestor up
            // also has a binding for that URI, this declaration is merged
            // away and the ancestor prefix wins; otherwise this prefix is
            // the effective binding.
            return match ancestor_uri_prefix(doc, node.parent, uri) {
                Some(ancestor) => Some(ancestor),
                None => Some(prefix.clone()),
            };
        }
        current = node.parent;
    }
    None
}

/// Text escaping: `& < >`; a literal carriage return is written as
/// `&#13;`; quotes are not escaped.
fn escape_text<O: XmlOutput>(s: &str, out: &mut O) -> Result<(), O::Error> {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;")?,
            '<' => out.push_str("&lt;")?,
            '>' => out.push_str("&gt;")?,
            '\r' => out.push_str("&#13;")?,
            other => out.push(other)?,
        }
    }
    Ok(())
}

/// Attribute-value escaping: `& < > "`; tab/newline/carriage return are
/// written as character references.
fn escape_attr<O: XmlOutput>(s: &str, out: &mut O) -> Result<(), O::Error> {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;")?,
            '<' => out.push_str("&lt;")?,
            '>' => out.push_str("&gt;")?,
            '"' => out.push_str("&quot;")?,
            '\t' => out.push_str("&#9;")?,
            '\n' => out.push_str("&#10;")?,
            '\r' => out.push_str("&#13;")?,
            other => out.push(other)?,
        }
    }
    Ok(())
}
