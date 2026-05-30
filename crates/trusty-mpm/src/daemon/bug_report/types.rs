//! Shared types for the bug-reporting pipeline.
//!
//! Why: The multi-store aggregator, scrubber, preview builder, and GitHub
//!      filing client all exchange data through these stable, versioned types.
//!      Keeping them in one file decouples the pipeline stages from each other.
//! What: [`AggregatedError`] is the canonical representation after dedup-merge
//!       across multiple daemon stores. [`FilingResult`] carries the structured
//!       outcome of a GitHub filing call (create or comment).
//! Test: see the integration tests in `github.rs` and `multi_store.rs`.

use serde::{Deserialize, Serialize};
use trusty_common::error_capture::CapturedError;

/// A deduplicated error record aggregated across one or more daemon stores.
///
/// Why: Multiple daemons may capture the same error class (same fingerprint)
///      independently. The aggregator merges them by fingerprint so the preview
///      builder and filing client see one canonical record per unique error.
/// What: Wraps the most-recent [`CapturedError`] for this fingerprint, plus an
///       occurrence count accumulated across all stores that contributed records
///       with the same fingerprint.
/// Test: `multi_store::tests::dedup_merges_by_fingerprint`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregatedError {
    /// The most-recent captured error record for this fingerprint.
    pub record: CapturedError,
    /// Total occurrence count across all contributing stores.
    pub occurrences: usize,
}

/// The structured result of one GitHub issue-filing attempt.
///
/// Why: the MCP `report_bug` tool and the HTTP `POST /api/v1/report-bug`
///      endpoint need a single typed response they can JSON-serialize uniformly.
/// What: `filed` is `true` when a GitHub API call succeeded (either create or
///       comment). `deduped` is `true` when an existing open issue was found
///       and a "+1 occurrence" comment was posted instead of creating a new
///       issue. `issue_url` and `issue_number` reference the issue that was
///       created or incremented.
/// Test: `github::tests::filing_result_serializes_roundtrip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilingResult {
    /// `true` when the GitHub API call completed successfully.
    pub filed: bool,
    /// `true` when dedup was triggered — a comment was posted on an existing
    /// issue rather than creating a new one.
    pub deduped: bool,
    /// The HTML URL of the issue that was created or incremented.
    pub issue_url: String,
    /// The issue number in `bobmatnyc/trusty-tools`.
    pub issue_number: u64,
}

/// Request body for `POST /api/v1/report-bug`.
///
/// Why: sub-agents that cannot call MCP tools directly may POST to this HTTP
///      endpoint. The consent requirement is preserved — `confirm` must be
///      `true` or nothing is filed.
/// What: the fingerprint selects the error to file; `confirm` gates actual
///       filing (matching the MCP `report_bug` tool's `confirm` argument).
/// Test: covered by the daemon API tests in `api_tests.rs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportBugRequest {
    /// Fingerprint of the error to file (64-char hex SHA-256).
    pub fingerprint: String,
    /// Must be `true` for filing to proceed; `false` or absent → preview only.
    #[serde(default)]
    pub confirm: bool,
}

/// HTTP response body for `POST /api/v1/report-bug`.
///
/// Why: the HTTP endpoint response must be structured and JSON-serializable
///      for sub-agents that parse it programmatically.
/// What: wraps the filing outcome; when `confirm:false` or no token is
///       configured, `filed` is `false` and `note` carries an actionable
///       message.
/// Test: covered by the daemon API tests in `api_tests.rs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportBugResponse {
    /// `true` when the issue was actually filed or incremented.
    pub filed: bool,
    /// Present when `filed` is `true` — the structured filing outcome.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<FilingResult>,
    /// Human-readable note; present when `filed` is `false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}
