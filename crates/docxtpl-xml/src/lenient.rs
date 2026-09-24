//! 宽松（recover）解析器：逐条复刻 libxml2 6.1 recover 在本语料上的愈合规则。
//!
//! 规则总览（每个恢复动作产生一条 [`crate::Recovery`]）：
//! 1. 坏实体：丢弃 `&` + 名称字符（+可选分号），合法内建/数字引用照常解码；
//! 2. `<` 后非名称起始字符时只丢弃 `<`；
//! 3. 结束标签匹配祖先则自动闭合中间后代，否则当作栈顶元素的闭合；
//!    文档根被这样闭合后，剩余输入按根后杂散内容丢弃；
//! 4. 开标签 EOF/遇 `<` 按空元素自动闭合；属性畸形产出空元素并在故障点恢复扫描；
//! 5. 注释到 `-->`、CDATA 到 `]]>`、PI 到 `?>`，EOF 未闭合则整体丢弃；
//! 6. 根元素之后的杂散内容丢弃。
//!
//! 安全限额（深度、禁止 DTD）与严格模式同样强制。

use crate::error::XmlError;
use crate::input::{self, is_valid_char, line_col, skip_ws};
use crate::model::{ElementHead, NodeId, XmlDocument, XmlLimits};
use crate::names;
use crate::{ParseOutcome, Recovery, RecoveryKind};

/// 宽松解析入口。超深度、DTD 等安全错误仍返回 [`XmlError`]。
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

/// 开标签扫描结论。
enum TagAction {
    /// 正常 `>` 结束，压栈。
    Push,
    /// 正常 `/>` 结束，空元素不压栈。
    Void,
    /// 自动闭合（EOF 或在 `>` 前遇到 `<`），附带诊断类别。
    AutoClose(RecoveryKind),
    /// 属性畸形等：在指定偏移恢复文本扫描，元素以空元素落地。
    Resume(usize, RecoveryKind),
}

struct TagScan {
    raw_name: String,
    attrs: Vec<(String, String)>,
    action: TagAction,
    /// 触发恢复时的诊断偏移与详情。
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
        // XML 声明仅在文件起始处识别（允许前导 BOM）。
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
                return Err(XmlError::at(self.input, self.pos, "缺少文档根元素"));
            }
            Some('<') => {
                let lt = self.pos;
                self.pos += 1;
                match self.peek() {
                    Some(c) if names::is_name_start(c) => {
                        self.open_tag(lt)?;
                    }
                    _ => {
                        // 根前杂散 '<'：丢弃并继续寻找根元素。
                        self.diag(RecoveryKind::MalformedTag, lt, "根元素之前的孤立 <");
                    }
                }
            }
            Some(_) => {
                // 根元素之前的杂散文本：丢弃到下一个 '<'。
                let rest = &self.input[self.pos..];
                let rel = rest.find('<').unwrap_or(rest.len());
                if self.input[self.pos..self.pos + rel]
                    .chars()
                    .any(|c| !names::is_ws(c))
                {
                    self.diag(
                        RecoveryKind::PrologTailDropped,
                        self.pos,
                        "根元素之前的杂散文本",
                    );
                }
                self.pos += rel;
            }
        }
        Ok(())
    }

    /// 文档级注释（根前/根后）：消费但不入树（对齐 fromstring 只返回根元素）。
    fn consume_comment_doclevel(&mut self) {
        let start = self.pos;
        if let Some(rel) = self.input[self.pos + 4..].find("-->") {
            self.pos = self.pos + 4 + rel + 3;
        } else {
            self.diag(
                RecoveryKind::MalformedTag,
                start,
                "注释未闭合，丢弃至输入结尾",
            );
            self.pos = self.input.len();
        }
    }

    /// 文档级 PI：消费但不入树。
    fn consume_pi_doclevel(&mut self) {
        let start = self.pos;
        self.pos += 2;
        if !self.scan_pi_to_end() {
            self.diag(
                RecoveryKind::MalformedTag,
                start,
                "PI 未闭合，丢弃至输入结尾",
            );
            self.pos = self.input.len();
        }
    }

    fn handle_bang_doclevel(&mut self, at: usize) -> Result<(), XmlError> {
        if self.is_doctype_at(at) {
            return Err(XmlError::EntityForbidden("检测到 DOCTYPE 声明".into()));
        }
        self.diag(RecoveryKind::MalformedTag, at, "根元素之前的非法 <! 标记");
        self.pos += 1; // 只丢弃 '<'，下一轮把 '!' 当文本
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
                    "根元素后的未闭合注释",
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
                    "根元素后的未闭合 PI",
                );
                self.pos = self.input.len();
                return;
            }
            self.diag(
                RecoveryKind::PrologTailDropped,
                self.pos,
                "根元素之后的杂散内容",
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
                // EOF：按文档序自动闭合所有未闭合元素。
                while !self.stack.is_empty() {
                    self.flush();
                    self.diag(
                        RecoveryKind::TagAutoClosed,
                        self.pos,
                        "输入结束时元素仍未闭合",
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

    /// 处理内容中的 `<`（`pos` 指向 `<` 之后的字符）。
    fn handle_lt(&mut self, lt_pos: usize) -> Result<(), XmlError> {
        match self.peek() {
            None => {
                self.diag(RecoveryKind::MalformedTag, lt_pos, "EOF 前的孤立 <，已丢弃");
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
                    return Err(XmlError::EntityForbidden("检测到 DOCTYPE 声明".into()));
                } else {
                    self.diag(
                        RecoveryKind::MalformedTag,
                        lt_pos,
                        "无法识别的 <! 标记，仅丢弃 <",
                    );
                    // pos 留在 '!'，下一轮按文本扫描。
                }
            }
            Some('?') => self.handle_pi(lt_pos)?,
            Some(c) if names::is_name_start(c) => self.open_tag(lt_pos)?,
            Some(_) => {
                self.diag(
                    RecoveryKind::MalformedTag,
                    lt_pos,
                    "< 后不是合法名称起始字符，仅丢弃 <",
                );
                // pos 已跳过 '<'，后续字符按文本继续扫描。
            }
        }
        Ok(())
    }

    // ---- 元素 ----------------------------------------------------------

    /// 扫描并落地一个开标签。`lt_pos` 为 `<` 的偏移，`pos` 为名称首字符。
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

    /// 宽松扫描开标签，返回名称、属性与恢复动作，`pos` 最终停在恢复点。
    fn scan_tag_head(&mut self) -> TagScan {
        let lt_pos = self.pos - 1;
        let (raw_name, end) = match names::read_name(self.input, self.pos) {
            Some(v) => v,
            None => {
                return TagScan {
                    raw_name: String::new(),
                    attrs: Vec::new(),
                    action: TagAction::Resume(self.pos, RecoveryKind::MalformedTag),
                    diag: Some((lt_pos, "非法元素名".to_string())),
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
                            "开标签在 EOF 前未闭合，按空元素自动闭合".to_string(),
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
                        diag: Some((slash, "畸形的空元素结束符，按空元素处理".to_string())),
                    };
                }
                Some('<') => {
                    return TagScan {
                        raw_name: raw_name.to_string(),
                        attrs,
                        action: TagAction::AutoClose(RecoveryKind::TagAutoClosed),
                        diag: Some((
                            self.pos,
                            "遇到 > 之前先遇到 <，按空元素自动闭合".to_string(),
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
                                diag: Some((at, "非法属性名".to_string())),
                            };
                        }
                    };
                    self.pos = aend;
                    self.pos = skip_ws(self.input, self.pos);
                    if self.peek() != Some('=') {
                        // 无 = 的孤立属性名：libxml2 recover 忽略之，元素照常开启。
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
                                diag: Some((at, "属性值未用引号包裹，按空元素处理".to_string())),
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
                                    "属性值/开标签在 EOF 前未闭合，按空元素自动闭合".to_string(),
                                )),
                            }
                        }
                        AttrOutcome::LtBeforeQuote(at) => {
                            return TagScan {
                                raw_name: raw_name.to_string(),
                                attrs,
                                action: TagAction::Resume(at, RecoveryKind::MalformedTag),
                                diag: Some((at, "属性值内遇到 <，开标签按空元素处理".to_string())),
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
                        diag: Some((at, "开标签内非法字符，按空元素处理".to_string())),
                    };
                }
            }
        }
    }

    /// 扫描引号属性值（宽松解码实体、字面空白规范化为空格）。
    fn scan_attr_value(&mut self, quote: char) -> AttrValueScan {
        self.pos += 1; // 开引号
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

    /// 处理结束标签（`pos` 指向名称首字符或非名称字符）。
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
            // `</` 后不是名称：跳到 '>'，按栈顶元素的杂散闭合处理。
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
                // 先自动闭合其上方所有未闭合后代（保留已解析内容）。
                while self.stack.len() - 1 > idx {
                    self.flush();
                    self.diag(
                        RecoveryKind::TagAutoClosed,
                        lt_pos,
                        format!("祖先结束标签 {raw} 先于后代闭合，自动闭合后代"),
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
                        "畸形结束标签，当作栈顶元素的闭合".to_string()
                    } else {
                        format!("结束标签 </{raw}> 不在祖先栈中，当作栈顶元素的闭合")
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

    /// 取元素的词法全名（前缀:本地名）。
    fn full_name(&self, id: NodeId) -> String {
        let n = &self.doc.nodes[id.0 as usize];
        match n.qname.as_ref() {
            Some(q) if n.prefix.is_empty() => q.local.clone(),
            Some(q) => format!("{}:{}", n.prefix, q.local),
            None => String::new(),
        }
    }

    // ---- 注释 / CDATA / PI / 声明 -------------------------------------

    fn handle_comment(&mut self, start: usize) -> Result<(), XmlError> {
        self.pos += 3; // 当前指向 '!' 后的 '--'
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
                "注释未闭合，丢弃至输入结尾",
            );
            self.pos = self.input.len();
        }
        Ok(())
    }

    fn handle_cdata(&mut self, start: usize) -> Result<(), XmlError> {
        self.pos += 8; // 越过 '![CDATA['（'<' 已消费）
        if let Some(rel) = self.input[self.pos..].find("]]>") {
            let body = self.input[self.pos..self.pos + rel].to_string();
            feed_normalized(&mut self.buf, &body, &mut self.cr_pending);
            self.pos += rel + 3;
        } else {
            // CDATA 未闭合：只丢弃 CDATA 段内容到 EOF，此前已累积的文本保留。
            self.diag(
                RecoveryKind::MalformedTag,
                start,
                "CDATA 段未闭合，丢弃至输入结尾",
            );
            self.pos = self.input.len();
        }
        Ok(())
    }

    fn handle_pi(&mut self, start: usize) -> Result<(), XmlError> {
        // pos 指向 '?'
        let target_pos = self.pos + 1;
        let target = match names::read_name(self.input, target_pos) {
            Some((t, end)) => {
                self.pos = end;
                t.to_string()
            }
            None => {
                // `<?` 后不是合法目标名：只丢弃 `<?`，从下一字符恢复文本扫描。
                self.diag(RecoveryKind::MalformedTag, start, "PI 缺少目标名");
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
                    "PI 未闭合，丢弃至输入结尾",
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

    /// 扫描 PI 目标与内容到 `?>`（不建节点），返回是否找到闭合。
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
                "XML 声明未闭合，丢弃至输入结尾",
            );
            self.pos = self.input.len();
        }
    }

    // ---- 实体 ----------------------------------------------------------

    /// 处理文本/属性值中的 `&`（pos 指向 &），按探针规则吞食。
    fn consume_amp(&mut self) -> AmpOutcome {
        let start = self.pos;
        let bytes = self.input.as_bytes();
        if start + 1 >= bytes.len() {
            self.pos = self.input.len();
            self.diag(RecoveryKind::BadEntityDropped, start, "结尾处的裸 &");
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
                    format!("丢弃未定义实体 &{name};"),
                );
                AmpOutcome::Drop
            } else {
                // 内建实体缺分号也不识别（如 &amp 后随空格）。
                self.diag(
                    RecoveryKind::BadEntityDropped,
                    start,
                    format!("丢弃缺少分号的实体片段 &{name}"),
                );
                AmpOutcome::Drop
            }
        } else {
            self.pos = start + 1;
            self.diag(
                RecoveryKind::BadEntityDropped,
                start,
                "裸 & 后不是合法实体名，仅丢弃 &",
            );
            AmpOutcome::Drop
        }
    }

    /// 处理 `&#` 开头的数字引用（含各畸形分支，依据探针实证）。
    fn consume_amp_hash(&mut self, start: usize) -> AmpOutcome {
        if let Some((ch, end)) = input::try_numeric_ref(self.input, start + 1) {
            self.pos = end;
            if is_valid_char(ch) {
                return AmpOutcome::Char(ch);
            }
            self.diag(
                RecoveryKind::BadEntityDropped,
                start,
                format!("丢弃指向非法字符的引用 U+{:04X}", ch as u32),
            );
            return AmpOutcome::Drop;
        }
        let bytes = self.input.as_bytes();
        let after_hash = start + 2;
        let is_x = matches!(bytes.get(after_hash), Some(b'x') | Some(b'X'));
        if bytes.get(after_hash) == Some(&b';') {
            // `&#;`：整体丢弃。
            self.pos = after_hash + 1;
        } else if is_x
            && !matches!(
                bytes.get(after_hash + 1),
                Some(b'0'..=b'9') | Some(b'a'..=b'f') | Some(b'A'..=b'F')
            )
        {
            // `&#x` 后不是十六进制数字：只丢弃 `&#x`（探针 a&#xZZ;b → aZZ;b）。
            self.pos = after_hash + 1;
        } else {
            // 其他畸形数字引用：吞食 &# 与后续名称字符，再带一个可选分号。
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
            "丢弃畸形数字字符引用",
        );
        AmpOutcome::Drop
    }

    // ---- 杂项 ----------------------------------------------------------

    fn flush(&mut self) {
        if let Some(top) = self.stack.last().copied() {
            let text = std::mem::take(&mut self.buf);
            self.doc.push_text(top, &text);
        } else {
            self.buf.clear();
        }
    }

    /// 判断 `<` 起始偏移处是否为 DOCTYPE 声明（大小写不敏感）。
    fn is_doctype_at(&self, lt_pos: usize) -> bool {
        self.input
            .get(lt_pos + 2..lt_pos + 9)
            .is_some_and(|s| s.eq_ignore_ascii_case("DOCTYPE"))
    }
}

/// 属性值扫描结论。
enum AttrOutcome {
    Closed,
    Eof,
    LtBeforeQuote(usize),
}

struct AttrValueScan {
    value: String,
    outcome: AttrOutcome,
}

/// `&` 处理结论。
enum AmpOutcome {
    Char(char),
    Drop,
}

/// 追加原始字符数据并做 XML 行尾规范化（`\r\n`/`\r` → `\n`）。
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
