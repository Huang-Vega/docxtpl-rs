//! XML 解析错误类型。

/// XML 解析与安全策略相关的全部错误。
///
/// 可预期失败一律通过 [`Result`] 返回（见 ADR-004：库代码不允许
/// unwrap/expect/panic）。宽松（recover）解析只愈合结构性损坏，
/// 安全类错误（超深、DTD/外部实体）在两种模式下都返回错误。
#[derive(Debug, thiserror::Error)]
pub enum XmlError {
    /// 普通语法/良构性错误，带 1 起始的行列号与字节偏移。
    #[error("XML 解析失败（行 {line} 列 {col}）: {message}")]
    Parse {
        /// 1 起始行号（按 `\n` 计数）。
        line: usize,
        /// 1 起始列号（按 Unicode 标量计数）。
        col: usize,
        /// 从输入起始计算的字节偏移。
        offset: usize,
        /// 人类可读的错误详情。
        message: String,
    },
    /// 元素嵌套深度超过限额（默认见 [`crate::XmlLimits`]）。
    #[error("XML 嵌套超过 {0} 层")]
    NestingLimitExceeded(usize),
    /// 遇到禁止的 DTD 或外部/通用实体声明。
    #[error("禁止 DTD/外部实体: {0}")]
    EntityForbidden(String),
    /// 输入提前结束、缺少根元素等不完整情况。
    #[error("XML 不完整: {0}")]
    Incomplete(String),
}

impl XmlError {
    /// 在指定字节偏移处构造一个语法错误。
    pub(crate) fn at(input: &str, offset: usize, message: impl Into<String>) -> Self {
        let (line, col) = crate::input::line_col(input, offset);
        Self::Parse {
            line,
            col,
            offset,
            message: message.into(),
        }
    }
}
