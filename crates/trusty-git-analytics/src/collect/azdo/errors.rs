//! Error types for the Azure DevOps client.
//!
//! Why: centralises all `AzdoError` variants so both `client.rs` and
//! `pr_fetcher.rs` can import from one place without circular references.
//! What: defines `AzdoError` with `thiserror` derives and re-exports it via
//! the `azdo` module.
//! Test: covered indirectly by every client test that asserts on specific
//! error variants.

/// Errors returned by the Azure DevOps client.
#[derive(Debug, thiserror::Error)]
pub enum AzdoError {
    /// The method is not yet implemented. `phase` indicates the planned
    /// phase number (e.g. 6 for work items).
    #[error("not implemented: {method} is planned for Phase {phase}")]
    NotImplemented {
        /// Name of the method that would have performed work.
        method: String,
        /// Phase number in which this method will be implemented.
        phase: u32,
    },

    /// Credentials were rejected at the format-validation stage.
    #[error("invalid credentials: {0}")]
    InvalidCredentials(String),

    /// The configured URL is malformed or not an Azure DevOps URL.
    #[error("invalid URL: {0}")]
    InvalidUrl(String),

    /// HTTP request returned an unhandled status code.
    #[error("HTTP error {status}: {message}")]
    Http {
        /// HTTP status code.
        status: u16,
        /// Response body or reason phrase.
        message: String,
    },

    /// HTTP 401 — PAT is missing, malformed, or rejected by ADO.
    #[error("authentication failed (401): check PAT and organisation URL")]
    Unauthorized,

    /// HTTP 403 — PAT is valid but lacks scope for the requested resource.
    #[error("access denied (403): PAT lacks required scope")]
    Forbidden,

    /// HTTP 404 — the requested resource was not found.
    ///
    /// This can mean the organisation URL is wrong, the project does not
    /// exist, a referenced work item ID is missing, or credentials don't
    /// grant visibility. The endpoint context is not threaded through this
    /// variant — callers should consult the failing endpoint to disambiguate.
    #[error("resource not found (404): check organisation, project, and credentials")]
    NotFound,

    /// Transport-level failure (DNS, TLS, timeout, connection reset, ...).
    #[error("request error: {0}")]
    Request(#[from] reqwest::Error),

    /// Response body could not be parsed as the expected JSON shape.
    #[error("response parse error: {0}")]
    Parse(String),

    /// Azure DevOps configuration failed validation at fetcher
    /// construction time: both `project` and `projects` are empty/blank.
    ///
    /// Returning this from `AdoPrFetcher::new` is the load-bearing check
    /// that prevents a misconfigured fetcher from silently returning
    /// `Ok(None)` for every PR (issue #91 regression guard).
    #[error("invalid Azure DevOps configuration: {0}")]
    Config(String),
}
