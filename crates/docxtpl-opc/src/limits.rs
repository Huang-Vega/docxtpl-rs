//! 资源限额。

/// OPC 包读写限额（见 `docs/security-limits.md` 与 ADR-004）。
///
/// 全部字段公开，可按需调整；默认值按 P1 初值给出，后续用 corpus 基准校准。
/// 超出任何限额时打开/写出返回 [`crate::OpcError::LimitExceeded`]。
///
/// # 示例
///
/// ```
/// use docxtpl_opc::PackageLimits;
///
/// let limits = PackageLimits {
///     max_entries: 100,
///     ..PackageLimits::default()
/// };
/// assert_eq!(limits.max_entries, 100);
/// assert_eq!(PackageLimits::default().max_compression_ratio, 200);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageLimits {
    /// ZIP 条目数上限。
    pub max_entries: usize,
    /// 单条目解压后大小上限（字节）。
    pub max_entry_uncompressed: u64,
    /// 全部条目解压总量上限（字节）。
    pub max_total_uncompressed: u64,
    /// 压缩比上限（解压后/压缩后；Stored 条目天然不受限）。
    pub max_compression_ratio: u64,
    /// 写出包总大小上限（字节），由计数写包装器强制。
    pub max_output_size: u64,
}

impl Default for PackageLimits {
    fn default() -> Self {
        Self {
            max_entries: 1_000,
            max_entry_uncompressed: 32 * 1024 * 1024,
            max_total_uncompressed: 128 * 1024 * 1024,
            max_compression_ratio: 200,
            max_output_size: 128 * 1024 * 1024,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_values_match_adr004() {
        let limits = PackageLimits::default();
        assert_eq!(limits.max_entries, 1_000);
        assert_eq!(limits.max_entry_uncompressed, 32 * 1024 * 1024);
        assert_eq!(limits.max_total_uncompressed, 128 * 1024 * 1024);
        assert_eq!(limits.max_compression_ratio, 200);
        assert_eq!(limits.max_output_size, 128 * 1024 * 1024);
    }
}
