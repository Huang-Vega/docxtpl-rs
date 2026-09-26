//! Strict parser: a well-formed XML subset for the parts produced by
//! python-docx, with zero recovery.

use crate::error::XmlError;
use crate::input::{self, is_valid_char};
use crate::model::{ElementHead, NodeId, XmlDocument, XmlLimits};
use crate::names;

/// Strict parse entry point.
pub(crate) fn parse(input: &str, limits: &XmlLimits) -> Result<XmlDocument, XmlError> {
    let mut p = Parser {
        input,
        limits,
        doc: XmlDocument {
            nodes: Vec::new(),
            root: 0,
            has_decl: false,
            prefix_uris: Vec::new(),
        },
        pos: 0,
        cr_pending: false,
        has_root: false,
    };
    if p.input.starts_with('\u{FEFF}') {
        p.pos = '\u{FEFF}'.len_utf8();
    }
    p.parse_prolog()?;
    if !p.has_root {
        return Err(XmlError::at(input, p.pos, "missing document root element"));
    }
    p.parse_epilog()?;
    Ok(p.doc)
}

struct Parser<'a> {
    input: &'a str,
    limits: &'a XmlLimits,
    doc: XmlDocument,
    pos: usize,
    cr_pending: bool,
    has_root: bool,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn err(&self, msg: impl Into<String>) -> XmlError {
        XmlError::at(self.input, self.pos, msg)
    }

    fn starts_with(&self, lit: &str) -> bool {
        self.input[self.pos..].starts_with(lit)
    }

    /// Parse the prolog: XML declaration, comments, PIs, whitespace; reject
    /// DOCTYPE.
    fn parse_prolog(&mut self) -> Result<(), XmlError> {
        // The XML declaration must come first (the BOM is skipped at entry).
        self.pos = input::skip_ws(self.input, self.pos);
        if self.starts_with("<?xml") {
            let after = self.pos + 5;
            match self.input[after..].chars().next() {
                Some(c) if names::is_ws(c) || c == '?' => self.parse_decl()?,
                _ => return Err(self.err("malformed XML declaration")),
            }
        }
        loop {
            self.pos = input::skip_ws(self.input, self.pos);
            if self.starts_with("<!--") {
                self.parse_comment(None)?;
            } else if self.starts_with("<?") {
                self.parse_pi(None)?;
            } else if self.starts_with("<!") {
                self.parse_bang()?;
            } else if self.peek() == Some('<') {
                self.pos += 1;
                self.parse_element(None, 1)?;
                self.has_root = true;
                return Ok(());
            } else if self.peek().is_none() {
                return Err(self.err("missing document root element"));
            } else {
                return Err(self.err("illegal characters before document root element"));
            }
        }
    }

    /// Only whitespace, comments and PIs are allowed after the root element.
    fn parse_epilog(&mut self) -> Result<(), XmlError> {
        loop {
            let before = self.pos;
            self.pos = input::skip_ws(self.input, self.pos);
            if self.starts_with("<!--") {
                self.parse_comment(None)?;
            } else if self.starts_with("<?") {
                self.parse_pi(None)?;
            } else if self.starts_with("<!") {
                self.parse_bang()?;
            } else if self.pos >= self.input.len() {
                return Ok(());
            } else {
                return Err(XmlError::at(
                    self.input,
                    before,
                    "illegal stray content after document root element",
                ));
            }
        }
    }

    /// Parse the `<?xml ... ?>` declaration and record its presence.
    fn parse_decl(&mut self) -> Result<(), XmlError> {
        let start = self.pos;
        match self.input[self.pos..].find("?>") {
            Some(rel) => {
                self.pos += rel + 2;
                self.doc.has_decl = true;
                Ok(())
            }
            None => Err(XmlError::at(
                self.input,
                start,
                "XML declaration not terminated",
            )),
        }
    }

    /// Handle `<!`: only comments/CDATA go through their own branches;
    /// everything else (including DOCTYPE) is rejected.
    ///
    /// Callers have inconsistent entry conventions (the prolog points at
    /// `<`, element content points at `!`); both positions are accepted
    /// here.
    fn parse_bang(&self) -> Result<(), XmlError> {
        let mut rest: &str = &self.input[self.pos..];
        rest = rest.strip_prefix('<').unwrap_or(rest);
        rest = rest.strip_prefix('!').unwrap_or(rest);
        if rest
            .as_bytes()
            .get(..7)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"DOCTYPE"))
        {
            let follows = rest[7..].chars().next();
            if follows.is_none_or(|c| names::is_ws(c) || c == '>') {
                return Err(XmlError::EntityForbidden(
                    "DOCTYPE declaration detected".to_string(),
                ));
            }
        }
        Err(XmlError::at(self.input, self.pos, "illegal <! declaration"))
    }

    /// Parse an element (opening tag, content, closing tag). `pos` points at
    /// the first character of the name.
    fn parse_element(&mut self, parent: Option<NodeId>, depth: usize) -> Result<NodeId, XmlError> {
        if depth > self.limits.max_depth {
            return Err(XmlError::NestingLimitExceeded(self.limits.max_depth));
        }
        let name_pos = self.pos;
        let (raw_name, after_name) = names::read_name(self.input, self.pos)
            .ok_or_else(|| self.err("invalid element name"))?;
        self.pos = after_name;
        let (raw_attrs, self_closing) = self.parse_tag_head()?;
        let head: ElementHead = self
            .doc
            .build_head(parent, raw_name, raw_attrs, true, name_pos, self.input)?;
        let id = self.doc.create_element(head);
        if let Some(p) = parent {
            self.doc.attach(p, id);
        }
        if self_closing {
            return Ok(id);
        }
        self.cr_pending = false;
        self.parse_content(id, depth)?;
        Ok(id)
    }

    /// Parse attributes inside an opening tag, consuming the closing `>` or
    /// `/>`; returns (attributes, whether self-closing).
    fn parse_tag_head(&mut self) -> Result<(Vec<(String, String)>, bool), XmlError> {
        let mut attrs = Vec::new();
        loop {
            self.pos = input::skip_ws(self.input, self.pos);
            match self.peek() {
                Some('>') => {
                    self.pos += 1;
                    return Ok((attrs, false));
                }
                Some('/') => {
                    self.pos += 1;
                    if self.peek() != Some('>') {
                        return Err(self.err("empty-element tag missing >"));
                    }
                    self.pos += 1;
                    return Ok((attrs, true));
                }
                Some(c) if names::is_name_start(c) => {
                    let (name, end) = names::read_name(self.input, self.pos)
                        .ok_or_else(|| self.err("invalid attribute name"))?;
                    self.pos = end;
                    self.pos = input::skip_ws(self.input, self.pos);
                    if self.peek() != Some('=') {
                        return Err(self.err("attribute missing = and value"));
                    }
                    self.pos += 1;
                    self.pos = input::skip_ws(self.input, self.pos);
                    let quote = match self.peek() {
                        Some(q @ ('"' | '\'')) => q,
                        _ => return Err(self.err("attribute value must be enclosed in quotes")),
                    };
                    self.pos += 1;
                    let value = self.parse_attr_value(quote)?;
                    attrs.push((name.to_string(), value));
                }
                Some(c) => {
                    return Err(self.err(format!("illegal character inside opening tag {c:?}")))
                }
                None => return Err(self.err("opening tag not terminated")),
            }
        }
    }

    /// Parse a quoted attribute value (entity decoding + whitespace
    /// normalization), consuming the closing quote.
    fn parse_attr_value(&mut self, quote: char) -> Result<String, XmlError> {
        let mut value = String::new();
        loop {
            match self.peek() {
                None => return Err(self.err("attribute value not terminated")),
                Some(c) if c == quote => {
                    self.pos += 1;
                    return Ok(value);
                }
                Some('&') => {
                    let (ch, end) = input::decode_entity_strict(self.input, self.pos)?;
                    self.pos = end;
                    value.push(ch);
                }
                Some('\t' | '\n' | '\r') => {
                    self.pos += 1;
                    value.push(' ');
                }
                Some('<') => return Err(self.err("< not allowed in attribute value")),
                Some(c) => {
                    self.pos += c.len_utf8();
                    if !is_valid_char(c) {
                        return Err(self.err(format!("illegal XML character U+{:04X}", c as u32)));
                    }
                    value.push(c);
                }
            }
        }
    }

    /// Parse element content up to the matching closing tag.
    fn parse_content(&mut self, id: NodeId, depth: usize) -> Result<(), XmlError> {
        loop {
            match self.peek() {
                None => return Err(self.err("element not closed")),
                Some('<') => {
                    let next_pos = self.pos + 1;
                    let c = self.input[next_pos..].chars().next();
                    match c {
                        Some('/') => {
                            self.pos = next_pos + 1;
                            return self.parse_close(id);
                        }
                        Some('!') => {
                            self.pos = next_pos;
                            if self.starts_with("<!--") {
                                self.parse_comment(Some(id))?;
                            } else if self.starts_with("<![CDATA[") {
                                self.parse_cdata(id)?;
                            } else {
                                self.parse_bang()?;
                            }
                        }
                        Some('?') => {
                            self.pos = next_pos;
                            self.parse_pi(Some(id))?;
                        }
                        Some(c) if names::is_name_start(c) => {
                            self.pos = next_pos;
                            self.parse_element(Some(id), depth + 1)?;
                        }
                        Some(_) => return Err(self.err("illegal < markup")),
                        None => return Err(self.err("EOF before element content ended")),
                    }
                }
                Some(_) => self.parse_text_until_lt(id)?,
            }
        }
    }

    /// Read a run of character data (decoding entities, validating
    /// characters, normalizing CR line endings).
    fn parse_text_until_lt(&mut self, id: NodeId) -> Result<(), XmlError> {
        let mut buf = String::new();
        loop {
            let rest = &self.input[self.pos..];
            let next = rest.find(['<', '&']);
            let (chunk, at_lt) = match next {
                Some(rel) => (&rest[..rel], rest.as_bytes()[rel] == b'<'),
                None => (rest, false),
            };
            self.feed_raw_chars(chunk, &mut buf)?;
            self.pos += chunk.len();
            match next {
                Some(_) if at_lt => break,
                Some(_) => {
                    let (ch, end) = input::decode_entity_strict(self.input, self.pos)?;
                    self.pos = end;
                    self.cr_pending = false;
                    if !is_valid_char(ch) {
                        return Err(self.err(format!(
                            "entity resolved to illegal character U+{:04X}",
                            ch as u32
                        )));
                    }
                    buf.push(ch);
                }
                None => break,
            }
        }
        self.doc.push_text(id, &buf);
        Ok(())
    }

    /// Validate characters one by one and normalize line endings (the
    /// pending-CR state is maintained across calls).
    fn feed_raw_chars(&mut self, chunk: &str, buf: &mut String) -> Result<(), XmlError> {
        for c in chunk.chars() {
            if !is_valid_char(c) {
                return Err(self.err(format!("illegal XML character U+{:04X}", c as u32)));
            }
            if c == '\n' && self.cr_pending {
                self.cr_pending = false;
                continue;
            }
            self.cr_pending = c == '\r';
            buf.push(if c == '\r' { '\n' } else { c });
        }
        Ok(())
    }

    /// Parse a closing tag; `pos` points at the first character of the
    /// name.
    fn parse_close(&mut self, id: NodeId) -> Result<(), XmlError> {
        let (name, end) = names::read_name(self.input, self.pos)
            .ok_or_else(|| self.err("invalid end tag name"))?;
        let open_name = self.doc.nodes[id.0 as usize]
            .qname
            .as_ref()
            .map(|q| {
                let p = &self.doc.nodes[id.0 as usize].prefix;
                if p.is_empty() {
                    q.local.clone()
                } else {
                    format!("{p}:{}", q.local)
                }
            })
            .unwrap_or_default();
        if name != open_name {
            return Err(self.err(format!(
                "end tag {name} does not match opening tag {open_name}"
            )));
        }
        self.pos = end;
        self.pos = input::skip_ws(self.input, self.pos);
        if self.peek() != Some('>') {
            return Err(self.err("end tag missing >"));
        }
        self.pos += 1;
        self.cr_pending = false;
        Ok(())
    }

    /// Parse a comment up to `-->`, adding it as a child of `parent`
    /// (discarded when parent is None).
    fn parse_comment(&mut self, parent: Option<NodeId>) -> Result<(), XmlError> {
        let start = self.pos;
        self.pos += 4; // <!--
        let rel = self.input[self.pos..]
            .find("-->")
            .ok_or_else(|| XmlError::at(self.input, start, "comment not terminated"))?;
        let body = &self.input[self.pos..self.pos + rel];
        let mut value = String::new();
        self.cr_pending = false;
        self.feed_raw_chars(body, &mut value)?;
        self.pos += rel + 3;
        self.cr_pending = false;
        if let Some(p) = parent {
            self.doc.push_text(p, "");
            let cid = self.doc.alloc(crate::model::NodeKind::Comment);
            self.doc.nodes[cid.0 as usize].value = value;
            self.doc.attach(p, cid);
        }
        Ok(())
    }

    /// Parse a CDATA section, merging its content into a text node.
    fn parse_cdata(&mut self, parent: NodeId) -> Result<(), XmlError> {
        let start = self.pos;
        self.pos += 9; // <![CDATA[
        let rel = self.input[self.pos..]
            .find("]]>")
            .ok_or_else(|| XmlError::at(self.input, start, "CDATA section not terminated"))?;
        let body = &self.input[self.pos..self.pos + rel];
        let mut value = String::new();
        self.feed_raw_chars(body, &mut value)?;
        self.pos += rel + 3;
        self.cr_pending = false;
        self.doc.push_text(parent, &value);
        Ok(())
    }

    /// Parse a processing instruction.
    fn parse_pi(&mut self, parent: Option<NodeId>) -> Result<(), XmlError> {
        let start = self.pos;
        self.pos += 2; // <?
        let (target, end) = names::read_name(self.input, self.pos)
            .ok_or_else(|| XmlError::at(self.input, start, "PI missing target name"))?;
        if target.eq_ignore_ascii_case("xml") {
            return Err(XmlError::at(
                self.input,
                start,
                "PI target name xml is reserved",
            ));
        }
        self.pos = end;
        let rel = self.input[self.pos..]
            .find("?>")
            .ok_or_else(|| XmlError::at(self.input, start, "PI not terminated"))?;
        let raw_data = &self.input[self.pos..self.pos + rel];
        let data = if let Some(rest) = raw_data.strip_prefix(|c: char| names::is_ws(c)) {
            rest
        } else if raw_data.is_empty() {
            ""
        } else {
            return Err(XmlError::at(
                self.input,
                start,
                "PI target must be followed by whitespace",
            ));
        };
        for c in data.chars() {
            if !is_valid_char(c) {
                return Err(XmlError::at(
                    self.input,
                    start,
                    format!("illegal character in PI U+{:04X}", c as u32),
                ));
            }
        }
        self.pos += rel + 2;
        if let Some(p) = parent {
            self.doc.push_text(p, "");
            let pid = self.doc.alloc(crate::model::NodeKind::Pi);
            self.doc.nodes[pid.0 as usize].pi_target = target.to_string();
            self.doc.nodes[pid.0 as usize].value = data.to_string();
            self.doc.attach(p, pid);
        }
        Ok(())
    }
}
