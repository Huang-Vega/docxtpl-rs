//! 统一错误类型。

/// docxtpl-opc 的全部公开错误。
///
/// 可预期失败一律通过 [`Result`] 返回（代码规范：输入路径禁止 unwrap/expect/panic）。
///
/// # 示例
///
/// ```
/// use docxtpl_opc::OpcError;
///
/// let err = OpcError::PartNotFound {
///     uri: "word/missing.xml".to_string(),
/// };
/// assert!(err.to_string().contains("word/missing.xml"));
/// ```
#[derive(Debug, thiserror::Error)]
pub enum OpcError {
    /// 底层 IO 失败（打开/落盘文件等）。
    ///
    /// 失败情况：文件不存在、权限不足、磁盘写满等。
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    /// 读取 ZIP 失败（损坏、加密条目、CRC 不符等）。
    #[error("ZIP 读取失败: {detail}")]
    ZipRead {
        /// 错误详情。
        detail: String,
    },
    /// 写出 ZIP 失败。
    #[error("ZIP 写出失败: {detail}")]
    ZipWrite {
        /// 错误详情。
        detail: String,
    },
    /// 超出 [`crate::PackageLimits`] 资源限额。
    ///
    /// `kind` 取值：`"entries"`、`"entry_uncompressed"`、`"total_uncompressed"`、
    /// `"compression_ratio"`、`"output"`（见 `docs/security-limits.md`）。
    #[error("超出限额 {kind}: {value} > {max}")]
    LimitExceeded {
        /// 限额类别。
        kind: &'static str,
        /// 实际值（压缩比类为倍数，其余为字节数/条目数）。
        value: u64,
        /// 限额值。
        max: u64,
    },
    /// ZIP 条目名不是合法 part URI。
    ///
    /// 失败情况：空名、反斜杠、`..` 段、绝对路径（前导 `/` 或 `X:` 盘符）、纯 `.` 段。
    #[error("非法 part URI {uri:?}: {reason}")]
    InvalidUri {
        /// 被拒绝的条目名。
        uri: String,
        /// 拒绝原因。
        reason: String,
    },
    /// 重复条目。三口径：`"exact"`（含空段折叠）、`"case"`（大小写折叠）、
    /// `"percent"`（百分号解码折叠）。
    #[error("重复条目 {uri}（{scope} 口径冲突）")]
    DuplicateEntry {
        /// 后出现的（冲突）条目名。
        uri: String,
        /// 冲突口径。
        scope: &'static str,
    },
    /// [Content_Types].xml 无效。
    ///
    /// 失败情况：XML 非法、缺少必需属性、或普通 part 缺少 content type。
    #[error("Content Types 无效: {reason}")]
    InvalidContentTypes {
        /// 原因。
        reason: String,
    },
    /// 关系（.rels）无效。
    ///
    /// 失败情况：XML 非法、缺少必需属性、内部关系悬空、关系 Id 重复。
    #[error("关系无效: {reason}")]
    InvalidRelationships {
        /// 原因。
        reason: String,
    },
    /// 缺少必需 part。
    #[error("缺少必需 part: {uri}")]
    MissingPart {
        /// 缺失 part 的 URI。
        uri: String,
    },
    /// 按 URI 查找 part 失败（如 [`crate::Package::set_part_bytes`] 的目标不存在）。
    #[error("找不到 part: {uri}")]
    PartNotFound {
        /// 未找到的 URI。
        uri: String,
    },
    /// 包结构无效（缺少 [Content_Types].xml / _rels/.rels、根关系缺少
    /// officeDocument 等）。
    #[error("包结构无效: {reason}")]
    Malformed {
        /// 原因。
        reason: String,
    },
}
