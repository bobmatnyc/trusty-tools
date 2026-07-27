//! Error types for the `collect` module.
//!
//! `CollectError` aggregates failures from git operations, HTTP requests,
//! the core database layer, and identity resolution.

use thiserror::Error;

/// Top-level error type for collection-stage operations.
///
/// Why: collection spans git2, HTTP clients (GitHub / JIRA / Linear /
/// Bitbucket / Azure DevOps), the core DB layer, and identity resolution;
/// a single uniform error keeps the per-provider clients aligned.
/// What: `thiserror` enum with `From` impls for git2, reqwest, rusqlite,
/// std::io, and serde_json, plus domain variants for identity / config
/// failures.
/// Test: covered indirectly — every provider client and `GitCollector`
/// test that exercises an error path produces these variants.
#[derive(Debug, Error)]
pub enum CollectError {
    /// A `git2`/libgit2 error occurred during repository operations.
    #[error("git error: {0}")]
    Git(#[from] git2::Error),

    /// An HTTP transport or response error occurred.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// A core error bubbled up from the `core` module (DB, config, validation).
    #[error("core error: {0}")]
    Core(#[from] crate::core::TgaError),

    /// A direct `rusqlite` error from inline SQL in this module.
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    /// Identity resolution failed for the given context.
    #[error("identity resolution failed: {0}")]
    Identity(String),

    /// An underlying `std::io` error (file not found, permission denied, etc.).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization/deserialization failure.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// A configuration value required for this operation was missing.
    #[error("configuration error: {0}")]
    Config(String),

    /// A paged JIRA changelog walk finished holding fewer history entries
    /// than the server reported existed (issue #4084).
    ///
    /// Why its own variant: this is the one failure where the collector
    /// *has* data and it is silently wrong — a short history reads exactly
    /// like a complete one downstream. Naming the ticket and both counts in
    /// a dedicated variant makes the shortfall impossible to mistake for a
    /// generic transport hiccup, and impossible to swallow as a partial
    /// success.
    #[error(
        "incomplete JIRA changelog for {key}: JIRA reported {expected} history \
         entries but only {retrieved} could be retrieved"
    )]
    IncompleteChangelog {
        /// JIRA issue key whose changelog came up short, e.g. `PROJ-123`.
        key: String,
        /// Entry count the server reported via the paged bean's `total`.
        expected: u64,
        /// Entry count actually accumulated across the paged walk.
        retrieved: u64,
    },

    /// The remote asked us to slow down (HTTP 429 / 503).
    ///
    /// Distinguished from [`CollectError::Http`] so retry logic can honour a
    /// `Retry-After` hint that `reqwest`'s own error type cannot carry, and
    /// so a caller can tell "the server is throttling us" apart from "the
    /// request was wrong" (issue #3966).
    #[error("throttled by remote (HTTP {status}){}", match retry_after {
        Some(d) => format!("; Retry-After: {}s", d.as_secs()),
        None => String::new(),
    })]
    Throttled {
        /// HTTP status that triggered the classification.
        status: u16,
        /// Server-supplied `Retry-After`, when present and parseable.
        retry_after: Option<std::time::Duration>,
    },
}

/// Module-wide `Result` alias.
///
/// Why: keeps signatures compact across many provider clients.
/// What: alias for `std::result::Result<T, CollectError>`.
/// Test: exercised by every fallible function in `collect`.
pub type Result<T> = std::result::Result<T, CollectError>;
