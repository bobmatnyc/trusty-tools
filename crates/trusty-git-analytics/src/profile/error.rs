//! Error type for the contributor-profile pipeline.
//!
//! Why: the pipeline spans identity resolution, SQLite queries, period-trend
//! reporting, and git diff extraction. A dedicated enum lets a caller branch on
//! the failure kind — "this contributor is unknown" needs a different response
//! than "the database is unreadable" — without inspecting error strings.
//! What: defines [`ProfileError`] and the [`Result`] alias used throughout
//! `src/profile/`. Each variant wraps the underlying tga error unchanged so the
//! original cause survives.
//! Test: `selector_not_found_returns_error` covers `ContributorNotFound`;
//! `resolve_db_path_missing_home_is_not_configured` covers `DbNotConfigured`.

use thiserror::Error;

/// A failure in the contributor-profile pipeline.
///
/// Why: identity resolution, DB access, report queries, and git diff extraction
/// fail in ways a caller must tell apart — `ContributorNotFound` points the user
/// at `tga aliases list`, while `Db` means the database itself is unusable.
/// What: one variant per failure source, each carrying the underlying error.
/// `#[non_exhaustive]` so the LLM and GitHub passes (#5464, #5465) can add
/// variants without a MAJOR bump.
/// Test: see the module-level `Test:` pointers above.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProfileError {
    /// No identity in the tga `authors` table matched the caller's query.
    #[error(
        "contributor '{query}' not found in the tga database. \
         Try `tga aliases list` to see known identities, or provide the canonical email directly."
    )]
    ContributorNotFound {
        /// The name, email, or GitHub login the caller supplied.
        query: String,
    },

    /// No database path could be determined from flag, environment, or the
    /// per-user data directory.
    #[error(
        "tga database path is not configured. \
         Pass an explicit path, set TGA_DB, or run `tga collect` first."
    )]
    DbNotConfigured,

    /// A database-layer failure (open, migration, or query).
    #[error("tga database error: {0}")]
    Db(#[from] crate::core::TgaError),

    /// A report-layer failure, e.g. `query_author_period_trends`.
    #[error("tga report error: {0}")]
    Report(#[from] crate::report::errors::ReportError),

    /// A git failure while computing a commit diff.
    #[error("git error while sampling diffs: {0}")]
    Git(#[from] crate::collect::errors::CollectError),

    /// A configuration failure, e.g. an unusable window size.
    #[error("profile configuration error: {0}")]
    Config(String),

    /// An I/O failure, e.g. writing the report to an unwritable directory.
    #[error("I/O error in profile pipeline: {0}")]
    Io(#[from] std::io::Error),

    /// The period-review model could not be resolved or reached.
    ///
    /// #5464: raised by `PeriodReviewer::from_slug` when no credential resolves
    /// for the slug's provider family, or no adapter factory is registered for
    /// it. A failure DURING a period review is not this — that path is fail-safe
    /// and returns no findings.
    #[error("inference provider error: {0}")]
    Inference(#[from] trusty_common::inference::InferenceError),
}

/// `Result` specialised to [`ProfileError`].
pub type Result<T> = std::result::Result<T, ProfileError>;
