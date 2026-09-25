//! 渲染错误类型（代码规范 §3.2：分层、可定位、不吞上下文）。

use docxtpl_xml::XmlError;
use std::fmt;

/// 模板错误的稳定类别（具体消息可演进，类别用于兼容 oracle 错误分类）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateErrorKind {
    /// 模板语法错误（对齐 jinja2 `TemplateSyntaxError`）。
    Syntax,
    /// 变量/属性未定义（strict undefined 下）。
    Undefined,
    /// 图片字节无法识别（对齐 docxtpl `UnrecognizedImageError`，
    /// 由 InlineImage 的 probe/尺寸换算失败触发）。
    Image,
    /// 替换参数不合法（对齐 docxtpl 0.20.2 直接抛出的 Python
    /// `ValueError`，P7：`replace_pic` 注册的图片标识在模板中未命中）。
    InvalidArgument,
    /// 其他模板执行错误。
    Other,
}

impl TemplateErrorKind {
    /// 对齐 oracle report 的 Python 异常类名。
    #[must_use]
    pub fn oracle_exception(self) -> &'static str {
        match self {
            TemplateErrorKind::Syntax => "TemplateSyntaxError",
            TemplateErrorKind::Undefined => "UndefinedError",
            TemplateErrorKind::Image => "UnrecognizedImageError",
            TemplateErrorKind::InvalidArgument => "ValueError",
            TemplateErrorKind::Other => "TemplateError",
        }
    }
}

/// 渲染管线错误。
#[derive(Debug)]
pub enum RenderError {
    /// 渲染超过资源预算。
    Limit {
        /// 出错的 part 名。
        part: String,
        /// 限额类别。
        kind: &'static str,
        /// 允许的最大值。
        max: u64,
    },
    /// XML 解析/愈合/后处理失败。
    Xml {
        /// 出错的 part 名。
        part: String,
        /// 底层 XML 错误。
        source: XmlError,
    },

    /// 模板语法或求值错误。
    Template {
        /// 稳定类别。
        kind: TemplateErrorKind,
        /// 出错的 part 名。
        part: String,
        /// MiniJinja 报告的 1 起始行号（若有）。
        line: Option<usize>,
        /// 错误消息。
        message: String,
        /// 对齐上游 `exc.docx_context`：出错行附近去标签后的纯文本片段。
        context: Vec<String>,
    },
}

impl RenderError {
    /// 取错误的稳定类别（XML 错误为 None）。
    #[must_use]
    pub fn kind(&self) -> Option<TemplateErrorKind> {
        match self {
            RenderError::Template { kind, .. } => Some(*kind),
            RenderError::Xml { .. } | RenderError::Limit { .. } => None,
        }
    }

    /// 出错附近的文本上下文（对齐上游 docx_context）。
    #[must_use]
    pub fn context_lines(&self) -> &[String] {
        match self {
            RenderError::Template { context, .. } => context,
            RenderError::Xml { .. } | RenderError::Limit { .. } => &[],
        }
    }

    /// 出错的 part 名。
    #[must_use]
    pub fn part(&self) -> &str {
        match self {
            RenderError::Xml { part, .. }
            | RenderError::Template { part, .. }
            | RenderError::Limit { part, .. } => part,
        }
    }
}

impl std::error::Error for RenderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            RenderError::Xml { source, .. } => Some(source),
            RenderError::Template { .. } | RenderError::Limit { .. } => None,
        }
    }
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenderError::Limit { part, kind, max } => {
                write!(f, "渲染资源超限（part {part}，{kind} 最大 {max}）")
            }
            RenderError::Xml { part, source } => {
                write!(f, "XML 处理失败（part {part}）: {source}")
            }
            RenderError::Template {
                kind,
                part,
                line,
                message,
                ..
            } => {
                let line = line
                    .map(|l| l.to_string())
                    .unwrap_or_else(|| "?".to_string());
                write!(f, "模板错误（part {part}，行 {line}）: {kind:?}: {message}")
            }
        }
    }
}
