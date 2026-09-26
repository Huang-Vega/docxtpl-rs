//! Lenient (recover) parser: replicates, rule by rule, the healing behavior
//! of libxml2 6.1 recover as observed on this corpus.
//!
//! Rule overview (each recovery action produces one [`crate::Recovery`]):
//! 1. Bad entities: drop `&` + name characters (+ optional semicolon);
//!    valid built-in/numeric references are decoded as usual;
//! 2. When `<` is followed by a non-name-start character, drop only the `<`;
//! 3. An end tag matching an ancestor auto-closes the intervening
//!    descendants; otherwise it is treated as closing the top-of-stack
//!    element; once the document root is closed this way, remaining input is
//!    discarded as post-root stray content;
//! 4. An opening tag at EOF or hitting `<` is auto-closed as an empty
//!    element; malformed attributes produce an empty element and scanning
//!    resumes at the failure point;
//! 5. Comments run to `-->`, CDATA to `]]>`, PIs to `?>`; if unclosed at
//!    EOF, the whole construct is dropped;
//! 6. Stray content after the root element is dropped.
//!
//! Safety limits (depth, DTD forbidden) are enforced exactly as in strict
//! mode.

use crate::error::XmlError;
use crate::input::{self, is_valid_char, line_col, skip_ws};
use crate::model::{ElementHead, NodeId, XmlDocument, XmlLimits};
use crate::names;
use crate::{ParseOutcome, Recovery, RecoveryKind};

/// Lenient parse entry point. Security errors such as excessive depth or
/// DTD still return [`XmlError`].
pub(crate) fn parse(input: &str, limits: &XmlLimits) -> Result<ParseOutcome, XmlError> {
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
        stack: Vec::new(),
        buf: String::new(),
        cr_pending: false,
        diagnostics: Vec::new(),
        phase: Phase::Prolog,
        root_set: false,
    };
    if input.starts_with('\u{FEFF}') {
        p.pos = '\u{FEFF}'.len_utf8();
    }
    p.run()?;
    Ok(ParseOutcome {
        doc: p.doc,
        diagnostics: p.diagnostics,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Prolog,
    Body,
    Tail,
}

/// Result of scanning an opening tag.
enum TagAction {
    /// Normal `>` ending; push onto the stack.
    Push,
    /// Normal `/>` ending; empty element, not pushed.
    Void,
    /// Auto-closed (EOF or `<` encountered before `>`), with the diagnostic
    /// category.
    AutoClose(RecoveryKind),
    /// Malformed attributes etc.: resume text scanning at the given offset;
    /// the element lands as an empty element.
    Resume(usize, RecoveryKind),
}

struct TagScan {
    raw_name: String,
    attrs: Vec<(String, String)>,
    action: TagAction,
    /// Diagnostic offset and detail when recovery is triggered.
    diag: Option<(usize, String)>,
}

struct Parser<'a> {
    input: &'a str,
    limits: &'a XmlLimits,
    doc: XmlDocument,
    pos: usize,
    stack: Vec<NodeId>,
    buf: String,
    cr_pending: bool,
    diagnostics: Vec<Recovery>,
    phase: Phase,
    root_set: bool,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn starts_with(&self, lit: &str) -> bool {
        self.input[self.pos..].starts_with(lit)
    }

    fn diag(&mut self, kind: RecoveryKind, offset: usize, detail: impl Into<String>) {
        let (line, col) = line_col(self.input, offset);
        self.diagnostics.push(Recovery {
            kind,
            offset,
            line,
            col,
            detail: detail.into(),
        });
    }

    fn run(&mut self) -> Result<(), XmlError> {
        loop {
            match self.phase {
                Phase::Prolog => self.step_prolog()?,
                Phase::Body => self.step_body()?,
                Phase::Tail => {
                    self.scan_tail();
                    return Ok(());
                }
            }
        }
    }

    // ---- prolog / tail -------------------------------------------------

    fn step_prolog(&mut self) -> Result<(), XmlError> {
        // The XML declaration is recognized only at the very start of the
        // file (a leading BOM is allowed).
        if (self.pos == 0 || self.pos == '\u{FEFF}'.len_utf8()) && self.starts_with("<?xml") {
            let after = self.pos + 5;
            let c = self.input[after..].chars().next();
            if c.is_some_and(|c| names::is_ws(c) || c == '?') {
                self.consume_decl();
                return Ok(());
            }
        }
        let before = self.pos;
        self.pos = skip_ws(self.input, self.pos);
        if self.starts_with("<!--") {
            self.consume_comment_doclevel();
            return Ok(());
        }
        if self.starts_with("<?") {
            self.consume_pi_doclevel();
            return Ok(());
        }
        if self.starts_with("<!") {
            return self.handle_bang_doclevel(before);
        }
        match self.peek() {
            None => {
                return Err(XmlError::at(
                    self.input,
                    self.pos,
                    "missing document root element",
                ));
            }
            Some('<') => {
                let lt = self.pos;
                self.pos += 1;
                match self.peek() {
                    Some(c) if names::is_name_start(c) => {
                        self.open_tag(lt)?;
                    }
                    _ => {
                        // Stray '<' before the root: drop it and keep
                        // looking for the root element.
                        self.diag(
                            RecoveryKind::MalformedTag,
                            lt,
                            "stray < before root element",
                        );
                    }
                }
            }
            Some(_) => {
                // Stray text before the root element: discard up to the
                // next '<'.
                let rest = &self.input[self.pos..];
                let rel = rest.find('<').unwrap_or(rest.len());
                if self.input[self.pos..self.pos + rel]
                    .chars()
                    .any(|c| !names::is_ws(c))
                {
                    self.diag(
                        RecoveryKind::PrologTailDropped,
                        self.pos,
                        "stray text before root element",
                    );
                }
                self.pos += rel;
            }
        }
        Ok(())
    }

    /// Document-level comment (before/after the root): consumed but not
    /// added to the tree (matching fromstring returning only the root).
    fn consume_comment_doclevel(&mut self) {
        let start = self.pos;
        if let Some(rel) = self.input[self.pos + 4..].find("-->") {
            self.pos = self.pos + 4 + rel + 3;
        } else {
            self.diag(
                RecoveryKind::MalformedTag,
                start,
                "unterminated comment, dropping through end of input",
            );
            self.pos = self.input.len();
        }
    }

    /// Document-level PI: consumed but not added to the tree.
    fn consume_pi_doclevel(&mut self) {
        let start = self.pos;
        self.pos += 2;
        if !self.scan_pi_to_end() {
            self.diag(
                RecoveryKind::MalformedTag,
                start,
                "unterminated PI, dropping through end of input",
            );
            self.pos = self.input.len();
        }
    }

    fn handle_bang_doclevel(&mut self, at: usize) -> Result<(), XmlError> {
        if self.is_doctype_at(at) {
            return Err(XmlError::EntityForbidden(
                "DOCTYPE declaration detected".into(),
            ));
        }
        self.diag(
            RecoveryKind::MalformedTag,
            at,
            "illegal <! markup before root element",
        );
        self.pos += 1; // Drop only '<'; the next round treats '!' as text.
        Ok(())
    }

    fn scan_tail(&mut self) {
        loop {
            self.pos = skip_ws(self.input, self.pos);
            if self.pos >= self.input.len() {
                return;
            }
            if self.starts_with("<!--") {
                let start = self.pos;
                if let Some(rel) = self.input[self.pos + 4..].find("-->") {
                    self.pos = self.pos + 4 + rel + 3;
                    continue;
                }
                self.diag(
                    RecoveryKind::PrologTailDropped,
                    start,
                    "unterminated comment after root element",
                );
                self.pos = self.input.len();
                return;
            }
            if self.starts_with("<?") {
                let start = self.pos;
                self.pos += 2;
                if self.scan_pi_to_end() {
                    continue;
                }
                self.diag(
                    RecoveryKind::PrologTailDropped,
                    start,
                    "unterminated PI after root element",
                );
                self.pos = self.input.len();
                return;
            }
            self.diag(
                RecoveryKind::PrologTailDropped,
                self.pos,
                "stray content after root element",
            );
            self.pos = self.input.len();
            return;
        }
    }

    // ---- body ----------------------------------------------------------

    fn step_body(&mut self) -> Result<(), XmlError> {
        if self.stack.is_empty() {
            self.phase = Phase::Tail;
            return Ok(());
        }
        let rest = &self.input[self.pos..];
        let special = rest.find(['<', '&']);
        match special {
            None => {
                feed_normalized(&mut self.buf, rest, &mut self.cr_pending);
                self.pos = self.input.len();
                // EOF: auto-close all still-open elements in document order.
                while !self.stack.is_empty() {
                    self.flush();
                    self.diag(
                        RecoveryKind::TagAutoClosed,
                        self.pos,
                        "element still unclosed at end of input",
                    );
                    self.stack.pop();
                }
                self.phase = Phase::Tail;
            }
            Some(rel) => {
                let abs = self.pos + rel;
                let is_lt = self.input.as_bytes()[abs] == b'<';
                let chunk = &rest[..rel];
                feed_normalized(&mut self.buf, chunk, &mut self.cr_pending);
                self.pos = abs;
                if is_lt {
                    self.pos += 1;
                    self.handle_lt(abs)?;
                } else {
                    if let AmpOutcome::Char(ch) = self.consume_amp() {
                        self.cr_pending = false;
                        self.buf.push(ch);
                    }
                }
            }
        }
        Ok(())
    }

    /// Handle a `<` in content (`pos` points at the character after the
    /// `<`).
    fn handle_lt(&mut self, lt_pos: usize) -> Result<(), XmlError> {
        match self.peek() {
            None => {
                self.diag(
                    RecoveryKind::MalformedTag,
                    lt_pos,
                    "stray < before EOF, dropped",
                );
            }
            Some('/') => {
                self.pos += 1;
                self.handle_end_tag(lt_pos)?;
            }
            Some('!') => {
                if self.starts_with("!--") {
                    self.handle_comment(lt_pos)?;
                } else if self.starts_with("![CDATA[") {
                    self.handle_cdata(lt_pos)?;
                } else if self.is_doctype_at(lt_pos) {
                    return Err(XmlError::EntityForbidden(
                        "DOCTYPE declaration detected".into(),
                    ));
                } else {
                    self.diag(
                        RecoveryKind::MalformedTag,
                        lt_pos,
                        "unrecognized <! markup, dropping only <",
                    );
                    // Leave pos on '!'; the next round scans it as text.
                }
            }
            Some('?') => self.handle_pi(lt_pos)?,
            Some(c) if names::is_name_start(c) => self.open_tag(lt_pos)?,
            Some(_) => {
                self.diag(
                    RecoveryKind::MalformedTag,
                    lt_pos,
                    "character after < is not a legal name-start character, dropping only <",
                );
                // pos has already skipped '<'; following characters continue
                // to be scanned as text.
            }
        }
        Ok(())
    }

    // ---- elements -----------------------------------------------------

    /// Scan and land an opening tag. `lt_pos` is the offset of `<`; `pos`
    /// is at the first character of the name.
    fn open_tag(&mut self, lt_pos: usize) -> Result<(), XmlError> {
        let scan = self.scan_tag_head();
        let will_push = matches!(scan.action, TagAction::Push);
        if will_push && self.stack.len() >= self.limits.max_depth {
            return Err(XmlError::NestingLimitExceeded(self.limits.max_depth));
        }
        let parent = self.stack.last().copied();
        let head: ElementHead = self.doc.build_head(
            parent,
            &scan.raw_name,
            scan.attrs,
            false,
            lt_pos + 1,
            self.input,
        )?;
        let id = self.doc.create_element(head);
        if let Some(p) = parent {
            self.flush();
            self.doc.attach(p, id);
        }
        if !self.root_set {
            self.doc.root = id.0;
            self.root_set = true;
        }
        let action_kind = match scan.action {
            TagAction::Push => RecoveryKind::Other,
            TagAction::Void => RecoveryKind::Other,
            TagAction::AutoClose(k) => k,
            TagAction::Resume(_, k) => k,
        };
        if let Some((offset, detail)) = scan.diag {
            self.diag(action_kind, offset, detail);
        }
        match scan.action {
            TagAction::Push => {
                self.stack.push(id);
                self.cr_pending = false;
                self.phase = Phase::Body;
            }
            TagAction::Void | TagAction::AutoClose(_) => {
                self.cr_pending = false;
                if self.stack.is_empty() {
                    self.phase = Phase::Tail;
                }
            }
            TagAction::Resume(at, _) => {
                self.pos = at;
                self.cr_pending = false;
                if self.stack.is_empty() {
                    self.phase = Phase::Tail;
                }
            }
        }
        Ok(())
    }

    /// Leniently scan an opening tag, returning the name, attributes and
    /// recovery action; `pos` ends at the recovery point.
    fn scan_tag_head(&mut self) -> TagScan {
        let lt_pos = self.pos - 1;
        let (raw_name, end) = match names::read_name(self.input, self.pos) {
            Some(v) => v,
            None => {
                return TagScan {
                    raw_name: String::new(),
                    attrs: Vec::new(),
                    action: TagAction::Resume(self.pos, RecoveryKind::MalformedTag),
                    diag: Some((lt_pos, "invalid element name".to_string())),
                }
            }
        };
        let mut attrs: Vec<(String, String)> = Vec::new();
        self.pos = end;
        loop {
            self.pos = skip_ws(self.input, self.pos);
            match self.peek() {
                None => {
                    return TagScan {
                        raw_name: raw_name.to_string(),
                        attrs,
                        action: TagAction::AutoClose(RecoveryKind::TagAutoClosed),
                        diag: Some((
                            self.pos,
                            "opening tag unclosed before EOF, auto-closing as empty element"
                                .to_string(),
                        )),
                    }
                }
                Some('>') => {
                    self.pos += 1;
                    return TagScan {
                        raw_name: raw_name.to_string(),
                        attrs,
                        action: TagAction::Push,
                        diag: None,
                    };
                }
                Some('/') => {
                    let slash = self.pos;
                    self.pos += 1;
                    if self.peek() == Some('>') {
                        self.pos += 1;
                        return TagScan {
                            raw_name: raw_name.to_string(),
                            attrs,
                            action: TagAction::Void,
                            diag: None,
                        };
                    }
                    return TagScan {
                        raw_name: raw_name.to_string(),
                        attrs,
                        action: TagAction::Resume(slash, RecoveryKind::MalformedTag),
                        diag: Some((
                            slash,
                            "malformed empty-element terminator, treating as empty element"
                                .to_string(),
                        )),
                    };
                }
                Some('<') => {
                    return TagScan {
                        raw_name: raw_name.to_string(),
                        attrs,
                        action: TagAction::AutoClose(RecoveryKind::TagAutoClosed),
                        diag: Some((
                            self.pos,
                            "hit < before >, auto-closing as empty element".to_string(),
                        )),
                    }
                }
                Some(c) if names::is_name_start(c) => {
                    let (aname, aend) = match names::read_name(self.input, self.pos) {
                        Some(v) => v,
                        None => {
                            let at = self.pos;
                            return TagScan {
                                raw_name: raw_name.to_string(),
                                attrs,
                                action: TagAction::Resume(at, RecoveryKind::MalformedTag),
                                diag: Some((at, "invalid attribute name".to_string())),
                            };
                        }
                    };
                    self.pos = aend;
                    self.pos = skip_ws(self.input, self.pos);
                    if self.peek() != Some('=') {
                        // A standalone attribute name without '=': libxml2
                        // recover ignores it and opens the element anyway.
                        continue;
                    }
                    self.pos += 1;
                    self.pos = skip_ws(self.input, self.pos);
                    let quote = match self.peek() {
                        Some(q @ ('"' | '\'')) => q,
                        _ => {
                            let at = self.pos;
                            return TagScan {
                                raw_name: raw_name.to_string(),
                                attrs,
                                action: TagAction::Resume(at, RecoveryKind::MalformedTag),
                                diag: Some((at, "attribute value not enclosed in quotes, treating as empty element".to_string())),
                            };
                        }
                    };
                    let value_scan = self.scan_attr_value(quote);
                    attrs.push((aname.to_string(), value_scan.value));
                    match value_scan.outcome {
                        AttrOutcome::Closed => {}
                        AttrOutcome::Eof => {
                            return TagScan {
                                raw_name: raw_name.to_string(),
                                attrs,
                                action: TagAction::AutoClose(RecoveryKind::TagAutoClosed),
                                diag: Some((
                                    self.pos,
                                    "attribute value/opening tag unclosed before EOF, auto-closing as empty element".to_string(),
                                )),
                            }
                        }
                        AttrOutcome::LtBeforeQuote(at) => {
                            return TagScan {
                                raw_name: raw_name.to_string(),
                                attrs,
                                action: TagAction::Resume(at, RecoveryKind::MalformedTag),
                                diag: Some((at, "hit < inside attribute value, treating opening tag as empty element".to_string())),
                            }
                        }
                    }
                }
                Some(_) => {
                    let at = self.pos;
                    return TagScan {
                        raw_name: raw_name.to_string(),
                        attrs,
                        action: TagAction::Resume(at, RecoveryKind::MalformedTag),
                        diag: Some((
                            at,
                            "illegal character inside opening tag, treating as empty element"
                                .to_string(),
                        )),
                    };
                }
            }
        }
    }

    /// Scan a quoted attribute value (lenient entity decoding; literal
    /// whitespace is normalized to spaces).
    fn scan_attr_value(&mut self, quote: char) -> AttrValueScan {
        self.pos += 1; // opening quote
        let mut value = String::new();
        loop {
            match self.peek() {
                None => {
                    return AttrValueScan {
                        value,
                        outcome: AttrOutcome::Eof,
                    }
                }
                Some(c) if c == quote => {
                    self.pos += 1;
                    return AttrValueScan {
                        value,
                        outcome: AttrOutcome::Closed,
                    };
                }
                Some('<') => {
                    return AttrValueScan {
                        value,
                        outcome: AttrOutcome::LtBeforeQuote(self.pos),
                    }
                }
                Some('&') => match self.consume_amp() {
                    AmpOutcome::Char(ch) => value.push(ch),
                    AmpOutcome::Drop => {}
                },
                Some('\t' | '\n' | '\r') => {
                    self.pos += 1;
                    value.push(' ');
                }
                Some(c) => {
                    self.pos += c.len_utf8();
                    value.push(c);
                }
            }
        }
    }

    /// Handle an end tag (`pos` points at the first character of the name
    /// or a non-name character).
    fn handle_end_tag(&mut self, lt_pos: usize) -> Result<(), XmlError> {
        let valid_name = self.peek().is_some_and(names::is_name_start);
        let raw = if valid_name {
            let (name, end) = names::read_name(self.input, self.pos)
                .map(|(n, e)| (n.to_string(), e))
                .unwrap_or_default();
            self.pos = end;
            self.pos = skip_ws(self.input, self.pos);
            if self.peek() == Some('>') {
                self.pos += 1;
            } else if let Some(rel) = self.input[self.pos..].find('>') {
                self.pos += rel + 1;
            } else {
                self.pos = self.input.len();
            }
            name
        } else {
            // Not a name after `</`: skip to '>' and treat as a stray close
            // for the top-of-stack element.
            if let Some(rel) = self.input[self.pos..].find('>') {
                self.pos += rel + 1;
            } else {
                self.pos = self.input.len();
            }
            String::new()
        };

        let found = if valid_name {
            self.stack
                .iter()
                .enumerate()
                .rev()
                .find(|(_, id)| self.full_name(**id) == raw)
                .map(|(idx, _)| idx)
        } else {
            None
        };

        match found {
            Some(idx) => {
                // Auto-close every still-open descendant above it first
                // (preserving already-parsed content).
                while self.stack.len() - 1 > idx {
                    self.flush();
                    self.diag(
                        RecoveryKind::TagAutoClosed,
                        lt_pos,
                        format!("ancestor end tag {raw} closes before its descendants; auto-closing descendants"),
                    );
                    self.stack.pop();
                }
                self.flush();
                self.stack.pop();
            }
            None => {
                self.flush();
                self.diag(
                    RecoveryKind::StrayEndTag,
                    lt_pos,
                    if raw.is_empty() {
                        "malformed end tag, treated as closing the top-of-stack element".to_string()
                    } else {
                        format!("end tag </{raw}> is not on the ancestor stack, treated as closing the top-of-stack element")
                    },
                );
                self.stack.pop();
            }
        }
        self.cr_pending = false;
        if self.stack.is_empty() {
            self.phase = Phase::Tail;
        }
        Ok(())
    }

    /// Get the lexical full name of an element (prefix:local name).
    fn full_name(&self, id: NodeId) -> String {
        let n = &self.doc.nodes[id.0 as usize];
        match n.qname.as_ref() {
            Some(q) if n.prefix.is_empty() => q.local.clone(),
            Some(q) => format!("{}:{}", n.prefix, q.local),
            None => String::new(),
        }
    }

    // ---- comments / CDATA / PI / declaration ---------------------------

    fn handle_comment(&mut self, start: usize) -> Result<(), XmlError> {
        self.pos += 3; // currently at the '--' after '!'
        if let Some(rel) = self.input[self.pos..].find("-->") {
            let body = &self.input[self.pos..self.pos + rel];
            let mut value = String::new();
            let mut pending = false;
            feed_normalized(&mut value, body, &mut pending);
            self.pos += rel + 3;
            if let Some(top) = self.stack.last().copied() {
                self.flush();
                let cid = self.doc.alloc(crate::model::NodeKind::Comment);
                self.doc.nodes[cid.0 as usize].value = value;
                self.doc.attach(top, cid);
            }
        } else {
            self.diag(
                RecoveryKind::MalformedTag,
                start,
                "unterminated comment, dropping through end of input",
            );
            self.pos = self.input.len();
        }
        Ok(())
    }

    fn handle_cdata(&mut self, start: usize) -> Result<(), XmlError> {
        self.pos += 8; // skip '![CDATA[' ('<' already consumed)
        if let Some(rel) = self.input[self.pos..].find("]]>") {
            let body = self.input[self.pos..self.pos + rel].to_string();
            feed_normalized(&mut self.buf, &body, &mut self.cr_pending);
            self.pos += rel + 3;
        } else {
            // Unclosed CDATA: drop only the CDATA content through EOF; text
            // accumulated before it is retained.
            self.diag(
                RecoveryKind::MalformedTag,
                start,
                "unterminated CDATA section, dropping through end of input",
            );
            self.pos = self.input.len();
        }
        Ok(())
    }

    fn handle_pi(&mut self, start: usize) -> Result<(), XmlError> {
        // pos points at '?'
        let target_pos = self.pos + 1;
        let target = match names::read_name(self.input, target_pos) {
            Some((t, end)) => {
                self.pos = end;
                t.to_string()
            }
            None => {
                // Not a legal target name after `<?`: drop only `<?` and
                // resume text scanning at the next character.
                self.diag(RecoveryKind::MalformedTag, start, "PI missing target name");
                self.pos = target_pos;
                return Ok(());
            }
        };
        let close_rel = self.input[self.pos..].find("?>");
        match close_rel {
            None => {
                self.diag(
                    RecoveryKind::MalformedTag,
                    start,
                    "unterminated PI, dropping through end of input",
                );
                self.pos = self.input.len();
            }
            Some(rel) => {
                let raw = &self.input[self.pos..self.pos + rel];
                let data = raw
                    .strip_prefix(|c: char| names::is_ws(c))
                    .unwrap_or(raw)
                    .to_string();
                self.pos += rel + 2;
                if let Some(top) = self.stack.last().copied() {
                    self.flush();
                    let pid = self.doc.alloc(crate::model::NodeKind::Pi);
                    self.doc.nodes[pid.0 as usize].pi_target = target;
                    self.doc.nodes[pid.0 as usize].value = data;
                    self.doc.attach(top, pid);
                }
            }
        }
        Ok(())
    }

    /// Scan the PI target and content up to `?>` (without creating a node);
    /// returns whether a closing sequence was found.
    fn scan_pi_to_end(&mut self) -> bool {
        let target_pos = self.pos;
        let target_end = names::read_name(self.input, target_pos)
            .map(|(_, end)| end)
            .unwrap_or(target_pos);
        if let Some(rel) = self.input[target_end..].find("?>") {
            self.pos = target_end + rel + 2;
            true
        } else {
            false
        }
    }

    fn consume_decl(&mut self) {
        if let Some(rel) = self.input[self.pos..].find("?>") {
            self.pos += rel + 2;
            self.doc.has_decl = true;
        } else {
            self.diag(
                RecoveryKind::MalformedTag,
                self.pos,
                "unterminated XML declaration, dropping through end of input",
            );
            self.pos = self.input.len();
        }
    }

    // ---- entities -----------------------------------------------------

    /// Handle a `&` in text/attribute values (pos points at `&`), consuming
    /// it per the probed rules.
    fn consume_amp(&mut self) -> AmpOutcome {
        let start = self.pos;
        let bytes = self.input.as_bytes();
        if start + 1 >= bytes.len() {
            self.pos = self.input.len();
            self.diag(
                RecoveryKind::BadEntityDropped,
                start,
                "bare & at end of input",
            );
            return AmpOutcome::Drop;
        }
        let c1 = self.input[start + 1..].chars().next().unwrap_or('&');
        if c1 == '#' {
            self.consume_amp_hash(start)
        } else if names::is_name_start(c1) {
            let (name, end) = names::read_name(self.input, start + 1)
                .unwrap_or((&self.input[start + 1..], start + 1));
            self.pos = end;
            if self.peek() == Some(';') {
                self.pos += 1;
                if let Some(ch) = input::builtin_entity(name) {
                    return AmpOutcome::Char(ch);
                }
                self.diag(
                    RecoveryKind::BadEntityDropped,
                    start,
                    format!("dropping undefined entity &{name};"),
                );
                AmpOutcome::Drop
            } else {
                // A built-in entity without a semicolon is not recognized
                // either (e.g. &amp followed by a space).
                self.diag(
                    RecoveryKind::BadEntityDropped,
                    start,
                    format!("dropping entity fragment &{name} missing semicolon"),
                );
                AmpOutcome::Drop
            }
        } else {
            self.pos = start + 1;
            self.diag(
                RecoveryKind::BadEntityDropped,
                start,
                "bare & not followed by a legal entity name, dropping only &",
            );
            AmpOutcome::Drop
        }
    }

    /// Handle a numeric reference starting with `&#` (including the various
    /// malformed branches, based on empirical probing).
    fn consume_amp_hash(&mut self, start: usize) -> AmpOutcome {
        if let Some((ch, end)) = input::try_numeric_ref(self.input, start + 1) {
            self.pos = end;
            if is_valid_char(ch) {
                return AmpOutcome::Char(ch);
            }
            self.diag(
                RecoveryKind::BadEntityDropped,
                start,
                format!(
                    "dropping reference to illegal character U+{:04X}",
                    ch as u32
                ),
            );
            return AmpOutcome::Drop;
        }
        let bytes = self.input.as_bytes();
        let after_hash = start + 2;
        let is_x = matches!(bytes.get(after_hash), Some(b'x') | Some(b'X'));
        if bytes.get(after_hash) == Some(&b';') {
            // `&#;`: drop the whole thing.
            self.pos = after_hash + 1;
        } else if is_x
            && !matches!(
                bytes.get(after_hash + 1),
                Some(b'0'..=b'9') | Some(b'a'..=b'f') | Some(b'A'..=b'F')
            )
        {
            // `&#x` not followed by a hex digit: drop only `&#x`
            // (probe a&#xZZ;b → aZZ;b).
            self.pos = after_hash + 1;
        } else {
            // Other malformed numeric references: consume &# and following
            // name characters, plus one optional semicolon.
            self.pos = after_hash;
            while let Some(c) = self.peek() {
                if names::is_name_char(c) {
                    self.pos += c.len_utf8();
                } else {
                    break;
                }
            }
            if self.peek() == Some(';') {
                self.pos += 1;
            }
        }
        self.diag(
            RecoveryKind::BadEntityDropped,
            start,
            "dropping malformed numeric character reference",
        );
        AmpOutcome::Drop
    }

    // ---- misc ----------------------------------------------------------

    fn flush(&mut self) {
        if let Some(top) = self.stack.last().copied() {
            let text = std::mem::take(&mut self.buf);
            self.doc.push_text(top, &text);
        } else {
            self.buf.clear();
        }
    }

    /// Return whether the offset of `<` starts a DOCTYPE declaration
    /// (case-insensitive).
    fn is_doctype_at(&self, lt_pos: usize) -> bool {
        self.input
            .get(lt_pos + 2..lt_pos + 9)
            .is_some_and(|s| s.eq_ignore_ascii_case("DOCTYPE"))
    }
}

/// Result of scanning an attribute value.
enum AttrOutcome {
    Closed,
    Eof,
    LtBeforeQuote(usize),
}

struct AttrValueScan {
    value: String,
    outcome: AttrOutcome,
}

/// Outcome of handling a `&`.
enum AmpOutcome {
    Char(char),
    Drop,
}

/// Append raw character data with XML line-ending normalization
/// (`\r\n`/`\r` → `\n`).
fn feed_normalized(out: &mut String, chunk: &str, cr_pending: &mut bool) {
    for c in chunk.chars() {
        if c == '\n' && *cr_pending {
            *cr_pending = false;
            continue;
        }
        *cr_pending = c == '\r';
        out.push(if c == '\r' { '\n' } else { c });
    }
}
