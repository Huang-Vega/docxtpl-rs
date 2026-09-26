//! Unified error type.

/// All public errors of docxtpl-opc.
///
/// All expected failures are returned via [`Result`] (code rule: no
/// unwrap/expect/panic on input paths).
///
/// # Examples
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
    /// Underlying IO failure (opening/flushing files, etc.).
    ///
    /// Failure cases: file not found, insufficient permissions, disk full, etc.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    /// Failed to read the ZIP (corruption, encrypted entries, CRC mismatch, etc.).
    #[error("failed to read ZIP: {detail}")]
    ZipRead {
        /// Error detail.
        detail: String,
    },
    /// Failed to write the ZIP.
    #[error("failed to write ZIP: {detail}")]
    ZipWrite {
        /// Error detail.
        detail: String,
    },
    /// A [`crate::PackageLimits`] resource limit was exceeded.
    ///
    /// `kind` is one of `"entries"`, `"entry_uncompressed"`,
    /// `"total_uncompressed"`, `"compression_ratio"`, `"output"`
    /// (see `docs/security-limits.md`).
    #[error("limit exceeded ({kind}): {value} > {max}")]
    LimitExceeded {
        /// Limit kind.
        kind: &'static str,
        /// Actual value (a multiplier for ratio limits, otherwise a byte/entry count).
        value: u64,
        /// Limit value.
        max: u64,
    },
    /// The ZIP entry name is not a valid part URI.
    ///
    /// Failure cases: empty name, backslash, `..` segment, absolute path
    /// (leading `/` or an `X:` drive prefix), or a bare `.` segment.
    #[error("invalid part URI {uri:?}: {reason}")]
    InvalidUri {
        /// The rejected entry name.
        uri: String,
        /// Reason for rejection.
        reason: String,
    },
    /// Duplicate entry. The three scopes are `"exact"` (with empty-segment
    /// collapse), `"case"` (case folding), and `"percent"` (percent-decode folding).
    #[error("duplicate entry {uri} ({scope} scope conflict)")]
    DuplicateEntry {
        /// The later (conflicting) entry name.
        uri: String,
        /// Conflict scope.
        scope: &'static str,
    },
    /// `[Content_Types].xml` is invalid.
    ///
    /// Failure cases: malformed XML, missing required attributes, or a regular
    /// part without a content type.
    #[error("invalid Content Types: {reason}")]
    InvalidContentTypes {
        /// Reason.
        reason: String,
    },
    /// The relationships (.rels) are invalid.
    ///
    /// Failure cases: malformed XML, missing required attributes, dangling
    /// internal relationships, or duplicate relationship Ids.
    #[error("invalid relationships: {reason}")]
    InvalidRelationships {
        /// Reason.
        reason: String,
    },
    /// A required part is missing.
    #[error("missing required part: {uri}")]
    MissingPart {
        /// URI of the missing part.
        uri: String,
    },
    /// Looking up a part by URI failed (e.g. the target of
    /// [`crate::Package::set_part_bytes`] does not exist).
    #[error("part not found: {uri}")]
    PartNotFound {
        /// The URI that was not found.
        uri: String,
    },
    /// Invalid package structure (missing `[Content_Types].xml` / _rels/.rels,
    /// root relationships without an officeDocument entry, etc.).
    #[error("invalid package structure: {reason}")]
    Malformed {
        /// Reason.
        reason: String,
    },
}
