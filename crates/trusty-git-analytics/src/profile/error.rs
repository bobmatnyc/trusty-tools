//! Error types for the contributor-profile pipeline.
//!
//! Why: the pipeline spans DB queries, git diff extraction, and report-layer
//! calls, each with a different failure mode. A dedicated enum lets callers
//! pattern-match on the failure kind — a missing identity is a user error with
//! an actionable hint, a SQLite failure is not.
//! What: defines [`ProfileError`] and the [`Result`] alias used throughout
//! `profile::`.
//! Test: `crate::profile::selector::tests::selector_not_found_returns_error`
//! covers the `ContributorNotFound` hint text; the remaining variants are
//! exercised by the batch and diff-sampler tests.

use thiserror::Error;

/// Errors that can occur in the contributor-profile pipeline.
///
/// Why: typed variants let callers surface actionable messages — a
/// `ContributorNotFound` tells the user to run `tga aliases list`, while `Db`
/// surfaces the underlying SQLite failure verbatim.
/// What: covers identity resolution, database access, git diff extraction,
/// configuration, and report-layer failures. `#[non_exhaustive]` so the LLM
/// narrative pass (#5464) can add variants without a major version bump.
/// Test: `crate::profile::selector::tests::selector_not_found_returns_error`.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProfileError {
    /// The requested contributor could not be resolved to a canonical identity
    /// in the tga database. Carries a hint for the user.
    #[error(
        "contributor '{query}' not found in the tga database. \
         Try `tga aliases list` to see known identities, or provide the canonical email directly."
    )]
    ContributorNotFound {
        /// The name, email, or GitHub login the caller supplied.
        query: String,
    },

    /// A `rusqlite` / core DB-layer error occurred.
    #[error("tga database error: {0}")]
    Db(#[from] crate::core::TgaError),

    /// A report-layer error (`query_author_period_trends`, …).
    #[error("tga report error: {0}")]
    Report(#[from] crate::report::errors::ReportError),

    /// A git error while computing commit diffs.
    #[error("git error while sampling diffs: {0}")]
    Git(#[from] crate::collect::errors::CollectError),

    /// A configuration error (e.g. an invalid window size).
    #[error("profile configuration error: {0}")]
    Config(String),

    /// An I/O error (e.g. writing the report to an unwritable directory).
    #[error("I/O error in profile pipeline: {0}")]
    Io(#[from] std::io::Error),
}

/// Convenience `Result` alias for the profile pipeline.
///
/// Why: avoids repeating `Result<T, ProfileError>` at every call site.
/// What: aliases `std::result::Result<T, ProfileError>`.
/// Test: used transitively by every profile-pipeline function.
pub type Result<T> = std::result::Result<T, ProfileError>;
