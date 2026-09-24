//! 渲染主管线：patch → MiniJinja → resolve_listing → recover → fix → serialize。

use docxtpl_compat::{patch_xml, resolve_listing};
use docxtpl_xml::{Recovery, XmlDocument, XmlLimits};
use minijinja::{Environment, ErrorKind, UndefinedBehavior};
use regex::Regex;
use serde_json::Value as JsonValue;
use std::io::{self, Write};
use std::sync::OnceLock;

use crate::error::{RenderError, TemplateErrorKind};
use crate::fix_tables::{fix_docpr_ids, fix_tables};

const MAX_RENDERED_XML_BYTES: usize = 64 * 1024 * 1024;
const MAX_TEMPLATE_FUEL: u64 = 10_000_000;

struct LimitedOutput {
    bytes: Vec<u8>,
    max: usize,
    exceeded: bool,
}

impl Write for LimitedOutput {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() > self.max.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(io::Error::other("rendered XML size limit exceeded"));
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// 主文档 part 名（本阶段仅渲染主文档，header/footer 属 P5）。
pub const MAIN_PART: &str = "word/document.xml";

/// 渲染选项（代码规范 §3.3：影响输出的选项必须显式传入，不读环境变量）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderOptions {
    /// 是否自动转义。上游 `DocxTemplate.render(autoescape=False)` 默认 false。
    autoescape: bool,
}

impl RenderOptions {
    /// 与上游默认行为一致的兼容模式：`autoescape=false`、lenient undefined。
    #[must_use]
    pub fn compat() -> Self {
        Self { autoescape: false }
    }

    /// 开启 autoescape 的显式增强（独立选项，不影响兼容口径，ADR-003）。
    #[must_use]
    pub fn with_autoescape(mut self, enabled: bool) -> Self {
        self.autoescape = enabled;
        self
    }

    /// 当前 autoescape 设置。
    #[must_use]
    pub fn autoescape(self) -> bool {
        self.autoescape
    }
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self::compat()
    }
}

/// 一次渲染的结果：新 part XML 与 recover 愈合诊断（ADR-002：愈合不静默）。
#[derive(Debug)]
pub struct RenderOutcome {
    /// 渲染并后处理后的 XML 字符串。
    pub xml: String,
    /// recover 解析记录的全部愈合动作。
    pub recoveries: Vec<Recovery>,
}

/// `<w:p>` 前插换行（上游 render_xml_part 起手，仅为错误行号定位）。
fn newline_before_p() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"<w:p([ >])").expect("invalid regex"))
}

/// 渲染后移除插入的换行。
fn newline_before_p_remove() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\n<w:p([ >])").expect("invalid regex"))
}

/// 去 XML 标签（对齐上游错误上下文 `re.sub(r"<[^>]+>", "", line)`）。
fn strip_xml_tags() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"<[^>]+>").expect("invalid regex"))
}

/// 渲染一个 XML part（当前阶段即 word/document.xml）。
///
/// 输入是模板 part 原文，输出是可写回的 XML；语义见
/// docs/compatibility.md §2 与 ADR-002/003。
pub fn render_document_xml(
    src_xml: &str,
    context: &JsonValue,
    options: &RenderOptions,
) -> Result<RenderOutcome, RenderError> {
    // 1. patch_xml：13 步有界正则变换。
    let patched = patch_xml(src_xml);

    // 2. 每个段落前插换行（仅用于错误定位，渲染后撤销）。
    let prepared = newline_before_p()
        .replace_all(&patched, "\n<w:p${1}")
        .into_owned();

    // 3. MiniJinja 渲染（默认对齐 jinja2：lenient undefined、autoescape=false）。
    let env = build_jinja_env(options.autoescape());
    let rendered = render_inline(&env, &prepared, context, MAIN_PART)?;

    // 4. 撤销换行 + 还原 {_{ }_} 字面转义。
    let dst = newline_before_p_remove()
        .replace_all(&rendered, "<w:p${1}")
        .into_owned();
    let dst = dst
        .replace("{_{", "{{")
        .replace("}_}", "}}")
        .replace("{%_", "{%")
        .replace("%_}", "%}");

    // 5. resolve_listing：\n \t \a \f → br/tab/换段/分页。
    let dst = resolve_listing(&dst);
    if dst.len() > MAX_RENDERED_XML_BYTES {
        return Err(RenderError::Limit {
            part: MAIN_PART.to_string(),
            kind: "rendered_xml_bytes",
            max: MAX_RENDERED_XML_BYTES as u64,
        });
    }

    // 6. 宽松(recover)解析愈合（安全限额仍强制）。
    let outcome =
        XmlDocument::parse_lenient(&dst, &XmlLimits::default()).map_err(|e| RenderError::Xml {
            part: MAIN_PART.to_string(),
            source: e,
        })?;
    let mut doc = outcome.doc;

    // 7. fix_tables / fix_docpr_ids。
    fix_tables(&mut doc)?;
    fix_docpr_ids(&mut doc);

    Ok(RenderOutcome {
        xml: doc.serialize(),
        recoveries: outcome.diagnostics,
    })
}

/// 构造与上游一致的 Jinja 环境：lenient undefined（对齐 jinja2 默认
/// `Undefined`），autoescape 仅在显式开启时生效（HTML 规则转义）。
pub(crate) fn build_jinja_env(autoescape: bool) -> Environment<'static> {
    let mut env = Environment::new();
    env.set_fuel(Some(MAX_TEMPLATE_FUEL));
    env.set_undefined_behavior(UndefinedBehavior::Lenient);
    if autoescape {
        env.set_auto_escape_callback(|_| minijinja::AutoEscape::Html);
    }
    env
}

/// 渲染单个模板字符串；错误映射带 part 名与去标签上下文。
pub(crate) fn render_inline(
    env: &Environment,
    template_src: &str,
    context: &JsonValue,
    part: &'static str,
) -> Result<String, RenderError> {
    let template = env
        .template_from_str(template_src)
        .map_err(|e| map_jinja_error(&e, template_src, part))?;
    let mut output = LimitedOutput {
        bytes: Vec::new(),
        max: MAX_RENDERED_XML_BYTES,
        exceeded: false,
    };
    let rendered =
        template.render_captured_to(minijinja::Value::from_serialize(context), &mut output);
    if output.exceeded {
        return Err(RenderError::Limit {
            part: part.to_string(),
            kind: "rendered_xml_bytes",
            max: MAX_RENDERED_XML_BYTES as u64,
        });
    }
    if let Err(e) = rendered {
        if e.kind() == ErrorKind::OutOfFuel {
            return Err(RenderError::Limit {
                part: part.to_string(),
                kind: "template_fuel",
                max: env.fuel().unwrap_or(MAX_TEMPLATE_FUEL),
            });
        }
        return Err(map_jinja_error(&e, template_src, part));
    }
    String::from_utf8(output.bytes).map_err(|e| RenderError::Template {
        kind: TemplateErrorKind::Other,
        part: part.to_string(),
        line: None,
        message: e.to_string(),
        context: Vec::new(),
    })
}

/// 把 MiniJinja 错误映射为稳定的 [`RenderError`]，附带对齐上游 docx_context
/// 的去标签文本片段（出错行前 4 行起共 7 行）。
fn map_jinja_error(err: &minijinja::Error, prepared: &str, part: &str) -> RenderError {
    let kind = match err.kind() {
        ErrorKind::SyntaxError => TemplateErrorKind::Syntax,
        ErrorKind::UndefinedError => TemplateErrorKind::Undefined,
        _ => TemplateErrorKind::Other,
    };
    let lines: Vec<&str> = prepared.lines().collect();
    let context = if let Some(lineno) = err.line() {
        let start = lineno.saturating_sub(4);
        lines
            .get(start..)
            .unwrap_or(&[])
            .iter()
            .take(7)
            .map(|line| strip_xml_tags().replace_all(line, "").into_owned())
            .collect()
    } else {
        Vec::new()
    };
    RenderError::Template {
        kind,
        part: part.to_string(),
        line: err.line(),
        message: err.to_string(),
        context,
    }
}

#[cfg(test)]
mod limit_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn output_writer_stops_before_exceeding_limit() {
        let mut output = LimitedOutput {
            bytes: Vec::new(),
            max: 4,
            exceeded: false,
        };
        output.write_all(b"abcd").unwrap();
        assert!(output.write_all(b"e").is_err());
        assert!(output.exceeded);
        assert_eq!(output.bytes, b"abcd");
    }

    #[test]
    fn looping_template_exhausts_fuel() {
        let mut env = build_jinja_env(false);
        env.set_fuel(Some(100));
        let error = render_inline(
            &env,
            "{% for i in range(1000) %}x{% endfor %}",
            &json!({}),
            MAIN_PART,
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                RenderError::Limit {
                    kind: "template_fuel",
                    ..
                }
            ),
            "{error:?}"
        );
    }
}
