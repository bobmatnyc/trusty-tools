//! Bug-reporting pipeline for the trusty-mpm daemon.
//!
//! Why: trusty-* daemons encounter runtime errors that developers need in order
//!      to fix bugs, but those errors may contain sensitive data (paths, tokens,
//!      usernames) that must never leave the user's machine without explicit
//!      consent. This module implements the Phase 2 + Phase 3 + Phase 4 pipeline:
//!      aggregate → scrub → preview → consent gate → rate-limit → file.
//!
//! What: six sub-modules, each with a single responsibility:
//!   - [`types`] — shared data types (`AggregatedError`, `FilingResult`, …).
//!   - [`multi_store`] — reads JSONL stores from all known daemons, merges by
//!     fingerprint, and returns a ranked `Vec<AggregatedError>`.
//!   - [`scrubber`] — strips paths, tokens, JWTs, AWS/Google/Slack keys, PEM
//!     blocks, connection strings, and env secrets; returns a [`ScrubResult`]
//!     with a human-readable redaction summary.
//!   - [`preview`] — builds the scrubbed Markdown body + labels for a preview
//!     or filing (the preview body IS the filed body).
//!   - [`token`] — token resolution: PAT env/file → GitHub App (Phase 4).
//!     Resolution order documented in [`token`]. Providers implement
//!     [`token::TokenProvider`].
//!   - [`github`] — GitHub REST API client (trait + real reqwest impl + mock).
//!     Dedup search + create-vs-comment logic.
//!   - [`ratelimit`] — per-fingerprint stamp (24h re-file window) + per-hour
//!     cap (default 10 issues/hour); both persisted to config dir JSON files.
//!
//! Public re-exports for the MCP backend and HTTP handlers:
//!   - [`aggregate_errors`] — merge errors from all daemon stores.
//!   - [`build_preview`] — build the scrubbed issue preview.
//!   - [`file_issue`] — file (or increment) a GitHub issue; gated on token.
//!   - [`token::EnvFileTokenProvider`] — the default PAT/file token provider.
//!   - [`token::GithubAppTokenProvider`] — the GitHub App installation-token provider.
//!   - [`token::resolve_token`] — top-level resolution following documented order.
//!   - [`GithubFilingError`] — typed error for the filing path.
//!   - [`FilingResult`], [`ReportBugRequest`], [`ReportBugResponse`] — HTTP/MCP types.
//!   - [`ScrubResult`] — structured output of the scrubber with summary.
//!   - [`ratelimit::RateLimitGuard`] — composite rate-limit guard.
//!
//! Test: each sub-module carries its own `#[cfg(test)]` suite. Run with
//!       `cargo test -p trusty-mpm`.

pub mod github;
pub mod github_client;
pub mod multi_store;
pub mod preview;
pub mod ratelimit;
pub mod scrubber;
pub mod token;
pub mod types;

// ── Convenience re-exports ─────────────────────────────────────────────────────

pub use github::{GithubFilingError, extract_fingerprint, file_issue, file_issue_with};
// Re-export token providers from the token module (Phase 4).
pub use multi_store::{aggregate_errors, aggregate_errors_from_paths};
pub use preview::{IssuePreview, build_preview};
pub use ratelimit::{FilingDecision, RateLimitGuard};
pub use scrubber::{ScrubChange, ScrubResult, scrub, scrub_compat};
pub use token::{
    EnvFileTokenProvider, GithubAppConfig, GithubAppTokenProvider, TokenProvider, resolve_token,
};
pub use types::{AggregatedError, FilingResult, ReportBugRequest, ReportBugResponse};
