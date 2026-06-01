//! Error types for the contributor-profile pipeline.
//!
//! Why: the profile pipeline spans DB queries, git operations, and tga library
//! calls — each with different failure modes.  A dedicated error enum lets
//! callers pattern-match on the failure kind (e.g. missing identity vs. DB
//! failure vs. bad config) without leaking tga internal types into the
//! trusty-review API surface.
//! What: defines [`ProfileError`] and the [`Result`] alias used throughout
//! `src/profile/`.
//! Test: constructors and `Display` impls are covered by the selector / batch /
//! diff-sampler unit tests indirectly; the `not_found` variant is exercised by
//! `selector::tests::identity_not_found`.

use thiserror::Error;

/// Errors that can occur in the contributor-profile pipeline.
///
/// Why: typed variants allow callers to surface actionable messages — e.g.
/// `ContributorNotFound` tells the user to run `tga aliases list`, while
/// `Db` surfaces the underlying SQLite failure.
/// What: covers identity resolution, database access, git diff extraction,
/// configuration errors, and tga-layer report errors.
/// Test: each variant is exercised by at least one test in the selector,
/// batch, or diff-sampler modules.
#[derive(Debug, Error)]
pub enum ProfileError {
    /// The requested contributor could not be resolved to a canonical identity
    /// in the tga database.  Includes a hint for the user.
    #[error(
        "contributor '{query}' not found in the tga database. \
         Try `tga aliases list` to see known identities, or provide the canonical email directly."
    )]
    ContributorNotFound {
        /// The name, email, or GitHub login the caller supplied.
        query: String,
    },

    /// The tga database path is not set or does not exist.
    #[error(
        "tga database path is not configured. \
         Set --db / TRUSTY_REVIEW_TGA_DB, or run `tga collect` first."
    )]
    DbNotConfigured,

    /// A rusqlite / tga DB-layer error occurred.
    #[error("tga database error: {0}")]
    Db(#[from] tga::core::TgaError),

    /// A tga report-layer error (query_author_period_trends, etc.).
    #[error("tga report error: {0}")]
    Report(#[from] tga::report::errors::ReportError),

    /// A git2 error while computing commit diffs.
    #[error("git error while sampling diffs: {0}")]
    Git(#[from] tga::collect::errors::CollectError),

    /// A configuration error (e.g. invalid window size).
    #[error("profile configuration error: {0}")]
    Config(String),

    /// I/O error (e.g. reading a repo path that doesn't exist).
    #[error("I/O error in profile pipeline: {0}")]
    Io(#[from] std::io::Error),
}

/// Convenience `Result` alias for the profile pipeline.
///
/// Why: avoids repeating `Result<T, ProfileError>` at every call site.
/// What: aliases `std::result::Result<T, ProfileError>`.
/// Test: used transitively by all profile-pipeline functions.
pub type Result<T> = std::result::Result<T, ProfileError>;
