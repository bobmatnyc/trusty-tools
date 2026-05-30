//! Bug-reporting pipeline for the trusty-mpm daemon.
//!
//! Why: trusty-* daemons encounter runtime errors that developers need in order
//!      to fix bugs, but those errors may contain sensitive data (paths, tokens,
//!      usernames) that must never leave the user's machine without explicit
//!      consent. This module implements the Phase 2 + Phase 3 pipeline:
//!      aggregate → scrub → preview → consent gate → file.
//!
//! What: five sub-modules, each with a single responsibility:
//!   - [`types`] — shared data types (`AggregatedError`, `FilingResult`, …).
//!   - [`multi_store`] — reads JSONL stores from all known daemons, merges by
//!     fingerprint, and returns a ranked `Vec<AggregatedError>`.
//!   - [`scrubber`] — strips paths, tokens, JWTs, and env secrets from strings.
//!   - [`preview`] — builds the scrubbed Markdown body + labels for a preview
//!     or filing (the preview body IS the filed body).
//!   - [`github`] — GitHub REST API client (trait + real reqwest impl + mock).
//!     Token resolution order: env `TRUSTY_BUGREPORT_GITHUB_TOKEN` → file →
//!     `TRUSTY_BUGREPORT_TOKEN_FILE`. Graceful `NoToken` error when absent.
//!
//! Public re-exports for the MCP backend and HTTP handlers:
//!   - [`aggregate_errors`] — merge errors from all daemon stores.
//!   - [`build_preview`] — build the scrubbed issue preview.
//!   - [`file_issue`] — file (or increment) a GitHub issue; gated on token.
//!   - [`EnvFileTokenProvider`] — the default token provider.
//!   - [`GithubFilingError`] — typed error for the filing path.
//!   - [`FilingResult`], [`ReportBugRequest`], [`ReportBugResponse`] — HTTP/MCP types.
//!
//! Test: each sub-module carries its own `#[cfg(test)]` suite. Run with
//!       `cargo test -p trusty-mpm`.

pub mod github;
pub mod multi_store;
pub mod preview;
pub mod scrubber;
pub mod types;

// ── Convenience re-exports ─────────────────────────────────────────────────────

pub use github::{
    EnvFileTokenProvider, GithubFilingError, TokenProvider, extract_fingerprint, file_issue,
    file_issue_with,
};
pub use multi_store::{aggregate_errors, aggregate_errors_from_paths};
pub use preview::{IssuePreview, build_preview};
pub use scrubber::{ScrubChange, scrub};
pub use types::{AggregatedError, FilingResult, ReportBugRequest, ReportBugResponse};
