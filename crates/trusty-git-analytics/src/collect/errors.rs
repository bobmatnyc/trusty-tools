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
///
/// `#[non_exhaustive]` because `tga` is a published crate and this enum grows
/// a variant nearly every time a provider learns a new failure mode
/// (`Throttled` via #3966, `IncompleteChangelog` / `PagingBudgetExceeded` via
/// #4084). Without the attribute each of those is a SemVer-major break —
/// every downstream exhaustive `match` fails to compile with E0004 — which
/// forces a minor bump for what is genuinely a patch. Downstreams must carry
/// a wildcard arm; in-tree callers already do.
#[derive(Debug, Error)]
#[non_exhaustive]
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

    /// A GitHub REST call returned a non-success status, with its body kept.
    ///
    /// Why (#5465): [`Self::Http`] is what `error_for_status()` produces, and it
    /// discards the response body — which is where GitHub puts the only useful
    /// part of a write failure ("Resource not accessible by personal access
    /// token"). Without the body a token-scope problem is indistinguishable
    /// from any other 403, and the write path is exactly where that
    /// distinction decides what the operator has to fix.
    #[error("GitHub API error (HTTP {status}) for {endpoint}: {message}")]
    GithubApi {
        /// HTTP status returned by GitHub.
        status: u16,
        /// URL that was called, for the operator to reproduce.
        endpoint: String,
        /// GitHub's response body, truncated when long.
        message: String,
    },

    /// A find-or-create issue lookup could not see every match GitHub reported.
    ///
    /// Why (#5465): the alternative to erroring here is treating "not on the
    /// pages I read" as "does not exist", which opens a duplicate issue on a
    /// live tracker — and no re-run undoes that. Distinct from a transport
    /// failure: every request SUCCEEDED, the result set was simply larger than
    /// the paging budget, so the answer is unknown rather than absent.
    #[error(
        "GitHub issue search for `{query}` is inconclusive: {scanned} of {total} \
         match(es) were read within the paging budget, so an existing thread may \
         have been missed; refusing to create a possible duplicate"
    )]
    GithubSearchInconclusive {
        /// Search query that returned more than the walk could cover.
        query: String,
        /// Matches actually read across the walked pages.
        scanned: usize,
        /// Matches GitHub reported via `total_count`.
        total: u64,
    },

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

    /// A paged JIRA walk did not terminate within its page budget.
    ///
    /// Why: every termination condition on a paged walk trusts the server to
    /// honour `startAt` or to eventually return an empty page. One that
    /// replays the same page forever satisfies neither, and these walks run
    /// once per ticket across a window of up to 10,000 tickets. Surfacing a
    /// runaway as an error keeps it a bounded, named failure instead of a
    /// hang, and — unlike the fail-open shapes this module has repeatedly
    /// produced — it can never be mistaken for a complete result.
    #[error(
        "JIRA {endpoint} paging for {key} did not terminate within {pages} pages; \
         the server is not honouring `startAt`"
    )]
    PagingBudgetExceeded {
        /// Short name of the endpoint being walked, e.g. `changelog`.
        endpoint: &'static str,
        /// JIRA issue key whose walk ran away, e.g. `PROJ-123`.
        key: String,
        /// Page budget that was exhausted.
        pages: usize,
    },

    /// A Linear GraphQL call returned a non-success status, with its body kept.
    ///
    /// Why (#5665): Linear answers an absent issue with HTTP 200 and
    /// `data.issue: null`, so a non-2xx never means "issue not found" — it
    /// means the call failed. Folding both into `Ok(None)` made a rejected API
    /// key indistinguishable from a missing issue: a whole run against an
    /// invalid-but-present key made 369 rejected calls, wrote zero rows, and
    /// exited 0 with no diagnostic. The body is kept because Linear puts the
    /// only actionable text there ("You need to authenticate to access this
    /// operation.").
    #[error("Linear API error (HTTP {status}) for {identifier}: {message}")]
    LinearApi {
        /// HTTP status returned by Linear.
        status: u16,
        /// Issue identifier the call was for, e.g. `ENG-123`.
        identifier: String,
        /// Linear's response body, truncated when long.
        message: String,
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
