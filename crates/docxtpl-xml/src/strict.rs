//! 严格解析器：python-docx 产出的良构 XML 子集，零恢复。

use crate::error::XmlError;
use crate::input::{self, is_valid_char};
use crate::model::{ElementHead, NodeId, XmlDocument, XmlLimits};
use crate::names;

/// 严格解析入口。
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
        return Err(XmlError::at(input, p.pos, "缺少文档根元素"));
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

    /// 解析 prolog：XML 声明、注释、PI、空白；拒绝 DOCTYPE。
    fn parse_prolog(&mut self) -> Result<(), XmlError> {
        // XML 声明必须最先出现（BOM 已在入口跳过）。
        self.pos = input::skip_ws(self.input, self.pos);
        if self.starts_with("<?xml") {
            let after = self.pos + 5;
            match self.input[after..].chars().next() {
                Some(c) if names::is_ws(c) || c == '?' => self.parse_decl()?,
                _ => return Err(self.err("XML 声明格式错误")),
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
                return Err(self.err("缺少文档根元素"));
            } else {
                return Err(self.err("文档根元素之前存在非法字符"));
            }
        }
    }

    /// 根元素之后只允许空白、注释与 PI。
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
                    "文档根元素之后存在非法杂散内容",
                ));
            }
        }
    }

    /// 解析 `<?xml ... ?>` 声明并记录存在性。
    fn parse_decl(&mut self) -> Result<(), XmlError> {
        let start = self.pos;
        match self.input[self.pos..].find("?>") {
            Some(rel) => {
                self.pos += rel + 2;
                self.doc.has_decl = true;
                Ok(())
            }
            None => Err(XmlError::at(self.input, start, "XML 声明未结束")),
        }
    }

    /// 处理 `<!`：仅允许注释/CDATA 走各自分支，其余（含 DOCTYPE）一律拒绝。
    ///
    /// 调用方入口约定不统一（prolog 指向 `<`，元素内容指向 `!`），
    /// 这里两种位置都兼容。
    fn parse_bang(&self) -> Result<(), XmlError> {
        let mut rest: &str = &self.input[self.pos..];
        rest = rest.strip_prefix('<').unwrap_or(rest);
        rest = rest.strip_prefix('!').unwrap_or(rest);
        if rest.len() >= 7 && rest[..7].eq_ignore_ascii_case("DOCTYPE") {
            let follows = rest[7..].chars().next();
            if follows.is_none_or(|c| names::is_ws(c) || c == '>') {
                return Err(XmlError::EntityForbidden("检测到 DOCTYPE 声明".to_string()));
            }
        }
        Err(XmlError::at(self.input, self.pos, "非法的 <! 声明"))
    }

    /// 解析元素（开标签、内容、闭标签）。`pos` 指向名称首字符。
    fn parse_element(&mut self, parent: Option<NodeId>, depth: usize) -> Result<NodeId, XmlError> {
        if depth > self.limits.max_depth {
            return Err(XmlError::NestingLimitExceeded(self.limits.max_depth));
        }
        let name_pos = self.pos;
        let (raw_name, after_name) =
            names::read_name(self.input, self.pos).ok_or_else(|| self.err("非法元素名"))?;
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

    /// 解析开标签内属性，结束时消费 `>` 或 `/>`，返回 (属性, 是否自闭合)。
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
                        return Err(self.err("空元素标签缺少 >"));
                    }
                    self.pos += 1;
                    return Ok((attrs, true));
                }
                Some(c) if names::is_name_start(c) => {
                    let (name, end) = names::read_name(self.input, self.pos)
                        .ok_or_else(|| self.err("非法属性名"))?;
                    self.pos = end;
                    self.pos = input::skip_ws(self.input, self.pos);
                    if self.peek() != Some('=') {
                        return Err(self.err("属性缺少 = 与取值"));
                    }
                    self.pos += 1;
                    self.pos = input::skip_ws(self.input, self.pos);
                    let quote = match self.peek() {
                        Some(q @ ('"' | '\'')) => q,
                        _ => return Err(self.err("属性值必须用引号包裹")),
                    };
                    self.pos += 1;
                    let value = self.parse_attr_value(quote)?;
                    attrs.push((name.to_string(), value));
                }
                Some(c) => return Err(self.err(format!("开标签内非法字符 {c:?}"))),
                None => return Err(self.err("开标签未结束")),
            }
        }
    }

    /// 解析引号包裹的属性值（实体解码 + 空白规范化），消费闭引号。
    fn parse_attr_value(&mut self, quote: char) -> Result<String, XmlError> {
        let mut value = String::new();
        loop {
            match self.peek() {
                None => return Err(self.err("属性值未结束")),
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
                Some('<') => return Err(self.err("属性值中不允许出现 <")),
                Some(c) => {
                    self.pos += c.len_utf8();
                    if !is_valid_char(c) {
                        return Err(self.err(format!("非法 XML 字符 U+{:04X}", c as u32)));
                    }
                    value.push(c);
                }
            }
        }
    }

    /// 解析元素内容直到匹配的闭标签。
    fn parse_content(&mut self, id: NodeId, depth: usize) -> Result<(), XmlError> {
        loop {
            match self.peek() {
                None => return Err(self.err("元素未闭合")),
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
                        Some(_) => return Err(self.err("非法的 < 标记")),
                        None => return Err(self.err("元素内容未结束就遇到 EOF")),
                    }
                }
                Some(_) => self.parse_text_until_lt(id)?,
            }
        }
    }

    /// 读取一段字符数据（解码实体、校验字符合法性、CR 行尾规范化）。
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
                        return Err(self.err(format!("实体解出非法字符 U+{:04X}", ch as u32)));
                    }
                    buf.push(ch);
                }
                None => break,
            }
        }
        self.doc.push_text(id, &buf);
        Ok(())
    }

    /// 逐字符校验并做行尾规范化（跨调用维护 CR 悬挂状态）。
    fn feed_raw_chars(&mut self, chunk: &str, buf: &mut String) -> Result<(), XmlError> {
        for c in chunk.chars() {
            if !is_valid_char(c) {
                return Err(self.err(format!("非法 XML 字符 U+{:04X}", c as u32)));
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

    /// 解析闭标签，`pos` 指向名称首字符。
    fn parse_close(&mut self, id: NodeId) -> Result<(), XmlError> {
        let (name, end) =
            names::read_name(self.input, self.pos).ok_or_else(|| self.err("非法结束标签名"))?;
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
            return Err(self.err(format!("结束标签 {name} 与开标签 {open_name} 不匹配")));
        }
        self.pos = end;
        self.pos = input::skip_ws(self.input, self.pos);
        if self.peek() != Some('>') {
            return Err(self.err("结束标签缺少 >"));
        }
        self.pos += 1;
        self.cr_pending = false;
        Ok(())
    }

    /// 解析注释到 `-->`，作为 `parent` 的子节点（parent 为 None 时丢弃）。
    fn parse_comment(&mut self, parent: Option<NodeId>) -> Result<(), XmlError> {
        let start = self.pos;
        self.pos += 4; // <!--
        let rel = self.input[self.pos..]
            .find("-->")
            .ok_or_else(|| XmlError::at(self.input, start, "注释未结束"))?;
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

    /// 解析 CDATA 段，内容并入文本节点。
    fn parse_cdata(&mut self, parent: NodeId) -> Result<(), XmlError> {
        let start = self.pos;
        self.pos += 9; // <![CDATA[
        let rel = self.input[self.pos..]
            .find("]]>")
            .ok_or_else(|| XmlError::at(self.input, start, "CDATA 段未结束"))?;
        let body = &self.input[self.pos..self.pos + rel];
        let mut value = String::new();
        self.feed_raw_chars(body, &mut value)?;
        self.pos += rel + 3;
        self.cr_pending = false;
        self.doc.push_text(parent, &value);
        Ok(())
    }

    /// 解析处理指令。
    fn parse_pi(&mut self, parent: Option<NodeId>) -> Result<(), XmlError> {
        let start = self.pos;
        self.pos += 2; // <?
        let (target, end) = names::read_name(self.input, self.pos)
            .ok_or_else(|| XmlError::at(self.input, start, "PI 缺少目标名"))?;
        if target.eq_ignore_ascii_case("xml") {
            return Err(XmlError::at(self.input, start, "PI 目标名 xml 被保留"));
        }
        self.pos = end;
        let rel = self.input[self.pos..]
            .find("?>")
            .ok_or_else(|| XmlError::at(self.input, start, "PI 未结束"))?;
        let raw_data = &self.input[self.pos..self.pos + rel];
        let data = if let Some(rest) = raw_data.strip_prefix(|c: char| names::is_ws(c)) {
            rest
        } else if raw_data.is_empty() {
            ""
        } else {
            return Err(XmlError::at(self.input, start, "PI 目标后必须为空白"));
        };
        for c in data.chars() {
            if !is_valid_char(c) {
                return Err(XmlError::at(
                    self.input,
                    start,
                    format!("PI 中存在非法字符 U+{:04X}", c as u32),
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
