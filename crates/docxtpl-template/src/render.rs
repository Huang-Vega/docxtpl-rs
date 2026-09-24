//! 渲染主管线：patch → MiniJinja → resolve_listing → recover → fix → serialize。

use docxtpl_compat::{patch_xml, resolve_listing};
use docxtpl_xml::{Recovery, XmlDocument, XmlLimits};
use minijinja::{Environment, ErrorKind, UndefinedBehavior};
use regex::Regex;
use serde_json::Value as JsonValue;
use std::sync::OnceLock;

use crate::error::{RenderError, TemplateErrorKind};
use crate::fix_tables::{fix_docpr_ids, fix_tables};

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
    env.render_str(template_src, minijinja::Value::from_serialize(context))
        .map_err(|e| map_jinja_error(&e, template_src, part))
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
