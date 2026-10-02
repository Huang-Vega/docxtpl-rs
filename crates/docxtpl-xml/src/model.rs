//! In-memory tree model: [`XmlDocument`] and its Vec-arena nodes.

use std::collections::HashSet;

use crate::error::XmlError;
use crate::names;

/// Common namespace URI constants found in document.xml.
pub mod ns_uri {
    /// `w`: WordprocessingML main namespace.
    pub const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
    /// `wpc`: drawing canvas.
    pub const WPC: &str = "http://schemas.microsoft.com/office/word/2010/wordprocessingCanvas";
    /// `mc`: markup compatibility.
    pub const MC: &str = "http://schemas.openxmlformats.org/markup-compatibility/2006";
    /// `o`: legacy Office namespace.
    pub const O: &str = "urn:schemas-microsoft-com:office:office";
    /// `r`: relationships namespace.
    pub const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    /// `m`: OMML math.
    pub const M: &str = "http://schemas.openxmlformats.org/officeDocument/2006/math";
    /// `v`: VML.
    pub const V: &str = "urn:schemas-microsoft-com:vml";
    /// `wp14`: Word 2010 drawing extensions.
    pub const WP14: &str = "http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing";
    /// `wp`: DrawingML wordprocessing drawing.
    pub const WP: &str = "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing";
    /// `w10`: legacy Word extensions.
    pub const W10: &str = "urn:schemas-microsoft-com:office:word";
    /// `w14`: Word 2010 extensions.
    pub const W14: &str = "http://schemas.microsoft.com/office/word/2010/wordml";
    /// `wpg`: drawing group.
    pub const WPG: &str = "http://schemas.microsoft.com/office/word/2010/wordprocessingGroup";
    /// `wpi`: ink.
    pub const WPI: &str = "http://schemas.microsoft.com/office/word/2010/wordprocessingInk";
    /// `wne`: Word 2006 new features.
    pub const WNE: &str = "http://schemas.openxmlformats.org/word/2006/wordml";
    /// `wps`: shapes.
    pub const WPS: &str = "http://schemas.microsoft.com/office/word/2010/wordprocessingShape";
    /// `mo`: Mac Office extensions.
    pub const MO: &str = "http://schemas.microsoft.com/office/mac/office/2008/main";
    /// `mv`: Mac VML.
    pub const MV: &str = "urn:schemas-microsoft-com:mac:vml";
    /// `pic`: DrawingML picture.
    pub const PIC: &str = "http://schemas.openxmlformats.org/drawingml/2006/picture";
    /// `a`: DrawingML main namespace.
    pub const A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
    /// Namespace bound to the implicit `xml` prefix.
    pub const XML: &str = "http://www.w3.org/XML/1998/namespace";
    /// Reserved `xmlns` namespace.
    pub const XMLNS: &str = "http://www.w3.org/2000/xmlns/";
}

/// Arena node identifier.
///
/// Stable for the lifetime of the document; [`XmlDocument::detach`] never
/// compacts the arena, so previously obtained identifiers stay valid.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct NodeId(pub(crate) u32);

/// Qualified name: namespace URI + local name. An empty `ns` means no
/// namespace.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct QName {
    /// Namespace URI; an empty string means no namespace.
    pub ns: String,
    /// Local name (without prefix).
    pub local: String,
}

impl QName {
    /// Construct a qualified name.
    pub(crate) fn new(ns: impl Into<String>, local: impl Into<String>) -> Self {
        Self {
            ns: ns.into(),
            local: local.into(),
        }
    }
}

/// Node kind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NodeKind {
    /// Element node.
    Element,
    /// Text node (consecutive character data is merged).
    Text,
    /// Comment node.
    Comment,
    /// Processing instruction node.
    Pi,
}

/// XML parsing safety limits.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct XmlLimits {
    /// Maximum element nesting depth (the root element is at depth 1).
    pub max_depth: usize,
}

impl Default for XmlLimits {
    fn default() -> Self {
        Self { max_depth: 512 }
    }
}

/// Element opening-tag information collected during parsing.
pub(crate) struct ElementHead {
    /// Lexical prefix (empty string means no prefix).
    pub prefix: String,
    /// Resolved qualified name.
    pub qname: QName,
    /// Normal attributes (in input order); values are decoded character data.
    pub attrs: Vec<(QName, String)>,
    /// Lexical attribute prefixes parallel to `attrs` (empty string = none).
    pub attr_prefix: Vec<String>,
    /// xmlns declarations carried on the element's opening tag,
    /// (prefix, URI); an empty prefix is the default namespace.
    pub nsdecls: Vec<(String, String)>,
}

/// A single arena node.
#[derive(Debug)]
pub(crate) struct Node {
    pub(crate) kind: NodeKind,
    pub(crate) parent: Option<NodeId>,
    pub(crate) children: Vec<NodeId>,
    pub(crate) qname: Option<QName>,
    pub(crate) prefix: String,
    pub(crate) attrs: Vec<(QName, String)>,
    pub(crate) attr_prefix: Vec<String>,
    pub(crate) nsdecls: Vec<(String, String)>,
    pub(crate) value: String,
    pub(crate) pi_target: String,
}

impl Node {
    fn new(kind: NodeKind) -> Self {
        Self {
            kind,
            parent: None,
            children: Vec::new(),
            qname: None,
            prefix: String::new(),
            attrs: Vec::new(),
            attr_prefix: Vec::new(),
            nsdecls: Vec::new(),
            value: String::new(),
            pi_target: String::new(),
        }
    }
}

/// An editable XML document tree (stored in a Vec arena).
#[derive(Debug)]
pub struct XmlDocument {
    pub(crate) nodes: Vec<Node>,
    pub(crate) root: u32,
    pub(crate) has_decl: bool,
    /// Document-level prefix→URI master table (in first-occurrence order).
    pub(crate) prefix_uris: Vec<(String, String)>,
}

impl XmlDocument {
    /// Allocate a new node and return its identifier.
    pub(crate) fn alloc(&mut self, kind: NodeKind) -> NodeId {
        let id = u32::try_from(self.nodes.len()).unwrap_or(u32::MAX);
        self.nodes.push(Node::new(kind));
        NodeId(id)
    }

    /// Create an element node from pre-parsed opening-tag information
    /// (not yet attached to a parent).
    pub(crate) fn create_element(&mut self, head: ElementHead) -> NodeId {
        for (prefix, uri) in &head.nsdecls {
            if !self.prefix_uris.iter().any(|(p, _)| p == prefix) {
                self.prefix_uris.push((prefix.clone(), uri.clone()));
            }
        }
        let id = self.alloc(NodeKind::Element);
        let node = &mut self.nodes[id.0 as usize];
        node.qname = Some(head.qname);
        node.prefix = head.prefix;
        node.attrs = head.attrs;
        node.attr_prefix = head.attr_prefix;
        node.nsdecls = head.nsdecls;
        id
    }

    /// Append raw character data to `top`; merge into the last child if it
    /// is already a text node.
    pub(crate) fn push_text(&mut self, top: NodeId, text: &str) {
        if text.is_empty() {
            return;
        }
        let last = self.nodes[top.0 as usize].children.last().copied();
        if let Some(last_id) = last {
            if self.nodes[last_id.0 as usize].kind == NodeKind::Text {
                self.nodes[last_id.0 as usize].value.push_str(text);
                return;
            }
        }
        let id = self.alloc(NodeKind::Text);
        self.nodes[id.0 as usize].value.push_str(text);
        self.attach(top, id);
    }

    /// Attach an existing node as the last child of a parent.
    pub(crate) fn attach(&mut self, parent: NodeId, child: NodeId) {
        if let Some(old) = self.nodes[child.0 as usize].parent {
            self.nodes[old.0 as usize].children.retain(|&c| c != child);
        }
        self.nodes[child.0 as usize].parent = Some(parent);
        self.nodes[parent.0 as usize].children.push(child);
    }

    /// Resolve a namespace prefix: first check this element's own
    /// declarations, then walk up the parent chain.
    pub(crate) fn resolve_prefix(
        &self,
        node: Option<NodeId>,
        local_decls: &[(String, String)],
        prefix: &str,
    ) -> Option<String> {
        if prefix == "xml" {
            return Some(ns_uri::XML.to_string());
        }
        if let Some(uri) = local_decls
            .iter()
            .rev()
            .find(|(p, _)| p == prefix)
            .map(|(_, u)| u)
        {
            return Some(uri.clone());
        }
        let mut cur = node;
        while let Some(id) = cur {
            let n = &self.nodes[id.0 as usize];
            if let Some((_, uri)) = n.nsdecls.iter().rev().find(|(p, _)| p == prefix) {
                return Some(uri.clone());
            }
            cur = n.parent;
        }
        None
    }

    /// Build element opening-tag information from the raw (attribute name,
    /// value) sequence.
    ///
    /// When `strict` is true, an unbound prefix is an error; in lenient mode
    /// names with unbound prefixes are treated as having no namespace but
    /// keep their lexical prefix (matching libxml2 recover).
    pub(crate) fn build_head(
        &self,
        parent: Option<NodeId>,
        raw_name: &str,
        raw_attrs: Vec<(String, String)>,
        strict: bool,
        pos_for_error: usize,
        input: &str,
    ) -> Result<ElementHead, XmlError> {
        let mut nsdecls: Vec<(String, String)> = Vec::new();
        let mut attrs: Vec<(QName, String)> = Vec::new();
        let mut attr_prefix: Vec<String> = Vec::new();

        for (raw, value) in &raw_attrs {
            if let Some(rest) = raw.strip_prefix("xmlns:") {
                nsdecls.push((rest.to_string(), value.clone()));
            } else if raw == "xmlns" {
                nsdecls.push((String::new(), value.clone()));
            }
        }

        let (prefix, local) = split_qname(raw_name);
        let qname = self.resolve_qname(
            parent,
            &nsdecls,
            prefix,
            local,
            strict,
            input,
            pos_for_error,
        )?;

        if strict {
            for (raw, _) in &raw_attrs {
                if raw_attrs.iter().filter(|(n, _)| n == raw).count() > 1 {
                    return Err(XmlError::at(
                        input,
                        pos_for_error,
                        format!("element {raw_name} has duplicate attribute {raw}"),
                    ));
                }
            }
        }

        for (raw, value) in raw_attrs {
            if raw == "xmlns" || raw.starts_with("xmlns:") {
                continue;
            }
            let (aprefix, alocal) = split_qname(&raw);
            let aqname = if aprefix.is_empty() {
                QName::new("", alocal)
            } else {
                self.resolve_qname(
                    parent,
                    &nsdecls,
                    aprefix,
                    alocal,
                    strict,
                    input,
                    pos_for_error,
                )?
            };
            attrs.push((aqname, value));
            attr_prefix.push(aprefix.to_string());
        }

        Ok(ElementHead {
            prefix: prefix.to_string(),
            qname,
            attrs,
            attr_prefix,
            nsdecls,
        })
    }

    /// Resolve the prefix of a single QName.
    #[allow(clippy::too_many_arguments)]
    fn resolve_qname(
        &self,
        parent: Option<NodeId>,
        local_decls: &[(String, String)],
        prefix: &str,
        local: &str,
        strict: bool,
        input: &str,
        pos: usize,
    ) -> Result<QName, XmlError> {
        if prefix.is_empty() {
            let ns = self
                .resolve_prefix(parent, local_decls, "")
                .unwrap_or_default();
            return Ok(QName::new(ns, local));
        }
        match self.resolve_prefix(parent, local_decls, prefix) {
            Some(uri) => Ok(QName::new(uri, local)),
            None if strict => Err(XmlError::at(
                input,
                pos,
                format!("unbound namespace prefix {prefix}:{local}"),
            )),
            None => Ok(QName::new("", local)),
        }
    }

    /// Return the document's root element identifier.
    pub fn root(&self) -> NodeId {
        NodeId(self.root)
    }

    /// Return the element's qualified name; `None` for non-element nodes.
    pub fn tag(&self, id: NodeId) -> Option<&QName> {
        self.nodes[id.0 as usize].qname.as_ref()
    }

    /// Return the direct child identifiers in document order.
    pub fn children(&self, id: NodeId) -> &[NodeId] {
        &self.nodes[id.0 as usize].children
    }

    /// Return the parent identifier; `None` for the root or detached nodes.
    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.nodes[id.0 as usize].parent
    }

    /// Return the node itself and all its descendants (depth-first,
    /// document order).
    pub fn descendants(&self, id: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![id];
        while let Some(cur) = stack.pop() {
            out.push(cur);
            let kids = self.nodes[cur.0 as usize].children.clone();
            for k in kids.into_iter().rev() {
                stack.push(k);
            }
        }
        out
    }

    /// Return the node kind.
    pub fn node_kind(&self, id: NodeId) -> NodeKind {
        self.nodes[id.0 as usize].kind
    }

    /// Return the node value:
    /// character data for text nodes, the comment body for comments, and the
    /// data after the target for PIs. Element nodes return an empty string.
    pub fn node_value(&self, id: NodeId) -> &str {
        &self.nodes[id.0 as usize].value
    }

    /// Return an attribute value of an element, matched by namespace URI
    /// and local name.
    pub fn attr(&self, id: NodeId, ns: &str, local: &str) -> Option<&str> {
        self.nodes[id.0 as usize]
            .attrs
            .iter()
            .find(|(q, _)| q.ns == ns && q.local == local)
            .map(|(_, v)| v.as_str())
    }

    /// Return all attributes of an element (in input order).
    pub fn attrs(&self, id: NodeId) -> &[(QName, String)] {
        &self.nodes[id.0 as usize].attrs
    }

    /// Return the xmlns declarations carried on the element's opening tag
    /// (in input order): tuples of `(prefix, URI)`, an empty prefix denotes
    /// the default namespace declaration.
    pub fn ns_decls(&self, id: NodeId) -> &[(String, String)] {
        &self.nodes[id.0 as usize].nsdecls
    }

    /// Document-level prefix→URI lookup (used to validate newly created elements).
    pub fn prefix_uri(&self, prefix: &str) -> Option<&str> {
        self.prefix_uris
            .iter()
            .find(|(p, _)| p == prefix)
            .map(|(_, u)| u.as_str())
    }

    /// Set an attribute value; if the attribute already exists, replace it
    /// in place (preserving order), otherwise append it at the end.
    ///
    /// An empty `ns` means an attribute without a namespace; otherwise the
    /// serialization prefix is chosen from the document-level URI→prefix
    /// mapping (fix_tables only writes `w:` attributes).
    pub fn set_attr(&mut self, id: NodeId, ns: &str, local: &str, value: String) {
        let node = &mut self.nodes[id.0 as usize];
        if let Some(slot) = node
            .attrs
            .iter_mut()
            .find(|(q, _)| q.ns == ns && q.local == local)
        {
            slot.1 = value;
            return;
        }
        let prefix = if ns.is_empty() {
            String::new()
        } else {
            self.prefix_uris
                .iter()
                .find(|(_, u)| u == ns)
                .map(|(p, _)| p.clone())
                .unwrap_or_default()
        };
        node.attrs.push((QName::new(ns, local), value));
        node.attr_prefix.push(prefix);
    }

    /// Create a detached (unattached) element with the `w:` prefix.
    ///
    /// The attribute list is `(local name, value)`; the URI of the `w`
    /// prefix is taken from an existing `xmlns:w` declaration in the
    /// document; an error is returned when the document does not declare it.
    pub fn new_w_element(
        &mut self,
        local: &str,
        attrs: Vec<(String, String)>,
    ) -> Result<NodeId, XmlError> {
        self.new_prefixed_element("w", ns_uri::W, local, attrs)
    }

    /// Create a detached (unattached) element with any declared prefix.
    ///
    /// `prefix` must be declared in the document and bound to
    /// `expected_uri`; the attribute list is `(local name, value)` and
    /// attributes share the element's prefix.
    pub fn new_prefixed_element(
        &mut self,
        prefix: &str,
        expected_uri: &str,
        local: &str,
        attrs: Vec<(String, String)>,
    ) -> Result<NodeId, XmlError> {
        if !names::is_name_start(local.chars().next().unwrap_or('\u{0}'))
            || local.chars().skip(1).any(|c| !names::is_name_char(c))
        {
            return Err(XmlError::Parse {
                line: 0,
                col: 0,
                offset: 0,
                message: format!("invalid element local name {local:?}"),
            });
        }
        match self.prefix_uri(prefix) {
            Some(uri) if uri == expected_uri => {}
            _ => {
                return Err(XmlError::Parse {
                    line: 0,
                    col: 0,
                    offset: 0,
                    message: format!(
                        "prefix {prefix:?} ({expected_uri}) is not declared in the document; cannot create an element with that prefix"
                    ),
                });
            }
        }
        let mut pairs = Vec::with_capacity(attrs.len());
        let mut prefixes = Vec::with_capacity(attrs.len());
        for (alocal, value) in attrs {
            pairs.push((QName::new(expected_uri, alocal), value));
            prefixes.push(prefix.to_string());
        }
        let id = self.alloc(NodeKind::Element);
        let node = &mut self.nodes[id.0 as usize];
        node.qname = Some(QName::new(expected_uri, local));
        node.prefix = prefix.to_string();
        node.attrs = pairs;
        node.attr_prefix = prefixes;
        Ok(id)
    }

    /// Replace all children of the element with a single text node
    /// (matching python-docx's `element.text = ...`: an empty string still
    /// keeps an explicit text node, serializing as `<x></x>`, not `<x/>`).
    pub fn set_element_text(&mut self, id: NodeId, text: &str) {
        for child in std::mem::take(&mut self.nodes[id.0 as usize].children) {
            self.nodes[child.0 as usize].parent = None;
        }
        let text_id = self.alloc(NodeKind::Text);
        self.nodes[text_id.0 as usize].value = text.to_string();
        self.attach(id, text_id);
    }

    /// Direct text of the element (the value of its first text child;
    /// returns `None` when there is no text child).
    #[must_use]
    pub fn element_text(&self, id: NodeId) -> Option<&str> {
        self.nodes[id.0 as usize]
            .children
            .iter()
            .find(|c| self.nodes[c.0 as usize].kind == NodeKind::Text)
            .map(|c| self.nodes[c.0 as usize].value.as_str())
    }

    /// Append `child` as the last child of `parent`; if `child` already
    /// has a parent, it is detached from its old position first.
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) {
        self.attach(parent, child);
    }

    /// Insert `child` at child position `index` of `parent` (matching
    /// lxml's `element.insert(index, child)`); when `child` is already
    /// attached it is detached from its old position first, and an `index`
    /// past the child count degrades to an append.
    ///
    /// Used for Subdoc numbering merges (ADR-007): upstream `_insert_num`
    /// (before the last `w:num`) and `_insert_abstract_num` (before the
    /// first `w:num`/the root start) insert by index.
    pub fn insert_child_at(&mut self, parent: NodeId, index: usize, child: NodeId) {
        if let Some(old) = self.nodes[child.0 as usize].parent.take() {
            self.nodes[old.0 as usize].children.retain(|&c| c != child);
        }
        let slot = &mut self.nodes[parent.0 as usize];
        let index = index.min(slot.children.len());
        slot.children.insert(index, child);
        self.nodes[child.0 as usize].parent = Some(parent);
    }

    /// Remove the node from its parent (the node itself stays in the arena
    /// and can be re-attached).
    pub fn detach(&mut self, id: NodeId) {
        if let Some(parent) = self.nodes[id.0 as usize].parent.take() {
            self.nodes[parent.0 as usize].children.retain(|&c| c != id);
        }
    }

    /// Deep-copy an element subtree inside this document.
    ///
    /// The returned root is detached. Namespace declarations and lexical
    /// prefixes are preserved because the source and destination share the
    /// same document namespace table.
    pub fn deepcopy_element_within(&mut self, src_id: NodeId) -> Result<NodeId, XmlError> {
        if self.nodes.get(src_id.0 as usize).map(|n| n.kind) != Some(NodeKind::Element) {
            return Err(XmlError::Parse {
                line: 0,
                col: 0,
                offset: 0,
                message: "deepcopy_element_within supports element nodes only".to_string(),
            });
        }

        fn copy(document: &mut XmlDocument, source: NodeId, parent: Option<NodeId>) -> NodeId {
            let (kind, children, qname, prefix, attrs, attr_prefix, nsdecls, value, pi_target) = {
                let node = &document.nodes[source.0 as usize];
                (
                    node.kind,
                    node.children.clone(),
                    node.qname.clone(),
                    node.prefix.clone(),
                    node.attrs.clone(),
                    node.attr_prefix.clone(),
                    node.nsdecls.clone(),
                    node.value.clone(),
                    node.pi_target.clone(),
                )
            };
            let copied = document.alloc(kind);
            {
                let node = &mut document.nodes[copied.0 as usize];
                node.parent = parent;
                node.qname = qname;
                node.prefix = prefix;
                node.attrs = attrs;
                node.attr_prefix = attr_prefix;
                node.nsdecls = nsdecls;
                node.value = value;
                node.pi_target = pi_target;
            }
            for child in children {
                let copied_child = copy(document, child, Some(copied));
                document.nodes[copied.0 as usize]
                    .children
                    .push(copied_child);
            }
            copied
        }

        Ok(copy(self, src_id, None))
    }

    /// Deep-copy the element subtree rooted at `src_id` in document `src`
    /// into this document (matching the namespace self-adaptation semantics
    /// of lxml `deepcopy` + `append`).
    ///
    /// Returns the detached root of the copy (unattached); the caller is
    /// responsible for attaching it.
    ///
    /// Namespace handling has three steps:
    /// 1. Copy the whole subtree (preserving lexical prefixes, attributes,
    ///    and declarations carried on opening tags);
    /// 2. Find prefix uses inside the copied subtree (element or attribute
    ///    names) that cannot be resolved; their URI is taken directly from
    ///    the resolved qualified name stored on the node — exactly the
    ///    binding supplied by the source tree's ancestor chain;
    /// 3. Land the promoted bindings on the copy root. If the same lexical
    ///    prefix in the target document is already bound to another URI,
    ///    allocate a deterministic `nsN` prefix for that group of uses and
    ///    rewrite the element/attribute names accordingly, equivalent to
    ///    prefix self-adaptation on cross-tree append in lxml.
    pub fn deepcopy_element(
        &mut self,
        src: &XmlDocument,
        src_id: NodeId,
    ) -> Result<NodeId, XmlError> {
        if src.nodes.get(src_id.0 as usize).map(|n| n.kind) != Some(NodeKind::Element) {
            return Err(XmlError::Parse {
                line: 0,
                col: 0,
                offset: 0,
                message: "deepcopy_element supports element nodes only".to_string(),
            });
        }
        // 1. Copy the whole subtree.
        let copy_root = copy_subtree(self, src, src_id, None);

        // 2. Collect the specific use sites that need promoted bindings.
        // Collecting only prefix → URI is insufficient: the same lexical
        // prefix may denote different URIs in different ancestor scopes of
        // the source tree, and lxml renames the conflicting group
        // automatically.
        #[derive(Clone)]
        struct NamespaceUse {
            node: NodeId,
            attr_index: Option<usize>,
            prefix: String,
            uri: String,
        }

        let mut uses: Vec<NamespaceUse> = Vec::new();
        for cur in self.descendants(copy_root) {
            let node = &self.nodes[cur.0 as usize];
            if node.kind != NodeKind::Element {
                continue;
            }
            // Element-name prefix: the empty prefix only needs a
            // declaration when the element actually lands in the default
            // namespace.
            let elem_ns = node
                .qname
                .as_ref()
                .map(|q| q.ns.as_str())
                .unwrap_or_default();
            if !elem_ns.is_empty() && self.resolve_prefix(Some(cur), &[], &node.prefix).is_none() {
                uses.push(NamespaceUse {
                    node: cur,
                    attr_index: None,
                    prefix: node.prefix.clone(),
                    uri: elem_ns.to_string(),
                });
            }
            // Attribute-name prefix: prefix-less attributes never land in
            // the default namespace, nothing to do.
            for (i, aprefix) in node.attr_prefix.iter().enumerate() {
                if aprefix.is_empty() || aprefix == "xml" {
                    continue;
                }
                let ans = node.attrs[i].0.ns.as_str();
                if !ans.is_empty() && self.resolve_prefix(Some(cur), &[], aprefix).is_none() {
                    uses.push(NamespaceUse {
                        node: cur,
                        attr_index: Some(i),
                        prefix: aprefix.clone(),
                        uri: ans.to_string(),
                    });
                }
            }
        }

        // 3. Choose an available prefix for each (original prefix, URI) and
        // land it. Generated prefixes must also avoid declarations already
        // inside the copied subtree, or inner shadowing would leave the
        // rewritten QNames unbound again.
        let mut unavailable: HashSet<String> = self
            .prefix_uris
            .iter()
            .map(|(prefix, _)| prefix.clone())
            .collect();
        for cur in self.descendants(copy_root) {
            unavailable.extend(
                self.nodes[cur.0 as usize]
                    .nsdecls
                    .iter()
                    .map(|(prefix, _)| prefix.clone()),
            );
        }
        // Prefixes inherited from pruned ancestors are not yet in the
        // target's global table or the copied subtree declarations, but
        // some of them may happen to be named ns0/ns1. They must be
        // reserved before generating replacement prefixes, otherwise two
        // distinct URIs would get the same prefix and produce duplicate
        // xmlns declarations on the root.
        unavailable.extend(uses.iter().map(|usage| usage.prefix.clone()));

        let mut mappings: Vec<((String, String), String)> = Vec::new();
        let mut next_generated = 0usize;
        for usage in &uses {
            let key = (usage.prefix.clone(), usage.uri.clone());
            if mappings.iter().any(|(existing, _)| existing == &key) {
                continue;
            }

            let chosen = match self.prefix_uri(&usage.prefix) {
                Some(uri) if uri != usage.uri => loop {
                    let candidate = format!("ns{next_generated}");
                    next_generated += 1;
                    if unavailable.insert(candidate.clone()) {
                        break candidate;
                    }
                },
                _ if mappings.iter().any(|((prefix, _), chosen)| {
                    prefix == &usage.prefix && chosen == &usage.prefix
                }) =>
                {
                    loop {
                        let candidate = format!("ns{next_generated}");
                        next_generated += 1;
                        if unavailable.insert(candidate.clone()) {
                            break candidate;
                        }
                    }
                }
                _ => {
                    unavailable.insert(usage.prefix.clone());
                    usage.prefix.clone()
                }
            };
            mappings.push((key, chosen));
        }

        for usage in uses {
            let Some(chosen) = mappings
                .iter()
                .find(|((prefix, uri), _)| prefix == &usage.prefix && uri == &usage.uri)
                .map(|(_, chosen)| chosen.clone())
            else {
                return Err(XmlError::Parse {
                    line: 0,
                    col: 0,
                    offset: 0,
                    message: "internal error: incomplete namespace mapping for cross-document copy"
                        .to_string(),
                });
            };
            if let Some(index) = usage.attr_index {
                self.nodes[usage.node.0 as usize].attr_prefix[index] = chosen;
            } else {
                self.nodes[usage.node.0 as usize].prefix = chosen;
            }
        }

        for ((_, uri), chosen) in mappings {
            let root_node = &mut self.nodes[copy_root.0 as usize];
            if !root_node
                .nsdecls
                .iter()
                .any(|(prefix, existing_uri)| prefix == &chosen && existing_uri == &uri)
            {
                root_node.nsdecls.push((chosen.clone(), uri.clone()));
            }
            if self.prefix_uri(&chosen).is_none() {
                self.prefix_uris.push((chosen, uri));
            }
        }
        Ok(copy_root)
    }
}

/// Recursively copy a subtree: duplicate all lexical node information and
/// attach the copy to `parent`.
fn copy_subtree(
    dst: &mut XmlDocument,
    src: &XmlDocument,
    src_id: NodeId,
    parent: Option<NodeId>,
) -> NodeId {
    let sn = &src.nodes[src_id.0 as usize];
    let id = dst.alloc(sn.kind);
    let node = &mut dst.nodes[id.0 as usize];
    node.qname = sn.qname.clone();
    node.prefix = sn.prefix.clone();
    node.attrs = sn.attrs.clone();
    node.attr_prefix = sn.attr_prefix.clone();
    node.nsdecls = sn.nsdecls.clone();
    node.value = sn.value.clone();
    node.pi_target = sn.pi_target.clone();
    node.parent = parent;
    if let Some(p) = parent {
        dst.nodes[p.0 as usize].children.push(id);
    }
    for &child in &sn.children {
        copy_subtree(dst, src, child, Some(id));
    }
    id
}

/// Split a lexical qualified name `prefix:local`; with more than one colon
/// the first one splits the name (lenient tolerance).
pub(crate) fn split_qname(raw: &str) -> (&str, &str) {
    match raw.split_once(':') {
        Some((prefix, local)) => (prefix, local),
        None => ("", raw),
    }
}
