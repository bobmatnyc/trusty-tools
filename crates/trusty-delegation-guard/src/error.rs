//! The one error type every constructor, parser and validator returns.

use thiserror::Error;

/// Why: callers (the tm tap, the evaluator service) must tell a malformed
/// record from a refused foreign-project bundle without string matching.
/// What: one variant per rejection class; every variant carries enough context
/// to log without echoing record content.
/// Test: `unknown_top_level_key_is_rejected`, `bundle_for_a_from_b_input_is_refused`.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DgError {
    /// The JSON did not parse or did not match the schema (unknown key,
    /// missing field, wrong type).
    #[error("parse error: {0}")]
    Parse(String),
    /// The `schema` field names another schema version.
    #[error("schema mismatch: expected {expected}, found {found}")]
    SchemaMismatch {
        /// The version this crate accepts.
        expected: &'static str,
        /// The version the record carried.
        found: String,
    },
    /// A field that must be a lowercase 64-char sha256 hex digest is not.
    #[error("not a sha256 hex digest")]
    InvalidHash,
    /// A project id is empty, too long, or uses characters outside `[a-z0-9._-]`.
    #[error("invalid project id")]
    InvalidProjectId,
    /// A required free-text field is empty.
    #[error("field `{0}` must not be empty")]
    EmptyField(&'static str),
    /// A size limit from the design (§3) is exceeded.
    #[error("limit exceeded: {0}")]
    LimitExceeded(&'static str),
    /// A derived value (hash, key, count) does not match its inputs.
    #[error("derived value mismatch: {0}")]
    DerivedMismatch(&'static str),
    /// Two parts of one record contradict each other.
    #[error("inconsistent record: {0}")]
    Inconsistent(&'static str),
    /// #10892 §3 test 5: input from one project offered to another project's
    /// bundle or event.
    #[error("foreign project: expected {expected}, found {found}")]
    ForeignProject {
        /// The project the bundle or event is keyed to.
        expected: String,
        /// The project the offered input belongs to.
        found: String,
    },
    /// A verdict breaks a §4.5 post-validation rule; the service records it as
    /// `unavailable: malformed`.
    #[error("malformed verdict: {0}")]
    MalformedVerdict(&'static str),
}

impl From<serde_json::Error> for DgError {
    fn from(err: serde_json::Error) -> Self {
        Self::Parse(err.to_string())
    }
}

/// Crate-wide result alias.
pub type Result<T> = std::result::Result<T, DgError>;
