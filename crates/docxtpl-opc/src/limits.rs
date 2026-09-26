//! Resource limits.

/// Read/write limits for OPC packages (see `docs/security-limits.md` and ADR-004).
///
/// All fields are public and can be adjusted as needed; the defaults are the
/// initial P1 values and will be calibrated against a corpus benchmark later.
/// Exceeding any limit makes open/write return [`crate::OpcError::LimitExceeded`].
///
/// # Examples
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
    /// Maximum number of ZIP entries.
    pub max_entries: usize,
    /// Maximum uncompressed size of a single entry (bytes).
    pub max_entry_uncompressed: u64,
    /// Maximum total uncompressed size of all entries (bytes).
    pub max_total_uncompressed: u64,
    /// Maximum compression ratio (uncompressed/compressed; Stored entries are
    /// inherently exempt).
    pub max_compression_ratio: u64,
    /// Maximum total size of the written package (bytes), enforced by the
    /// counting writer wrapper.
    pub max_output_size: u64,
}

impl Default for PackageLimits {
    fn default() -> Self {
        const DEFAULT_DOCUMENT_BYTES: u64 = 600 * 1024 * 1024;
        Self {
            max_entries: 6_000,
            max_entry_uncompressed: DEFAULT_DOCUMENT_BYTES,
            max_total_uncompressed: DEFAULT_DOCUMENT_BYTES,
            max_compression_ratio: 200,
            max_output_size: DEFAULT_DOCUMENT_BYTES,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_values_match_adr004() {
        let limits = PackageLimits::default();
        assert_eq!(limits.max_entries, 6_000);
        assert_eq!(limits.max_entry_uncompressed, 600 * 1024 * 1024);
        assert_eq!(limits.max_total_uncompressed, 600 * 1024 * 1024);
        assert_eq!(limits.max_compression_ratio, 200);
        assert_eq!(limits.max_output_size, 600 * 1024 * 1024);
    }
}
