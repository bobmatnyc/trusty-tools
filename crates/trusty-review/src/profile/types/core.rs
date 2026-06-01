//! Core profile types: `ContributorProfile`, `PROFILE_VERSION`, and helpers.
//!
//! Why: the profile type is the top-level output of the profile pipeline; keeping
//! it separate from period/finding/token types makes each file focused and under
//! the 500-line cap.
//! What: defines `ContributorProfile`, `PROFILE_VERSION`, and the private
//! `now_iso8601` / `epoch_days_to_ymd` helpers.
//! Test: `contributor_profile_serde_roundtrip` and `token_cost_summary_defaults_to_zero`
//! in the parent `tests` module.

use serde::{Deserialize, Serialize};

use super::finding::LongitudinalFinding;
use super::period::PeriodBatch;
use super::token::TokenCostSummary;
use super::token::Trajectory;

/// The profile pipeline version emitted in `ContributorProfile::review_version`.
///
/// Why: version-stamps the profile so consumers can detect schema changes
/// and handle backward-compatibility migration.
/// What: a string constant injected into every new profile.
/// Test: asserted in `contributor_profile_serde_roundtrip`.
pub const PROFILE_VERSION: &str = "tr-profile-0.1";

/// Full longitudinal contributor profile produced by the profile pipeline.
///
/// Why: the profile pipeline's output must be self-contained and serialisable
/// so it can be stored in the review log, posted as a GitHub comment, or fed
/// into a downstream system (dashboards, manager summaries).
/// What: aggregates identity metadata, all period batches, synthesised
/// findings, strengths/weaknesses, a trajectory assessment, the LLM narrative,
/// and telemetry.  `review_version` is pinned to `"tr-profile-0.1"` for this
/// pass.
/// Test: see `contributor_profile_serde_roundtrip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContributorProfile {
    // ── Identity ──────────────────────────────────────────────────────────
    /// Canonical email (as stored in tga `authors.canonical_email`).
    pub canonical_email: String,

    /// Canonical display name (as stored in tga `authors.canonical_name`).
    pub canonical_name: String,

    /// GitHub login, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub github_login: Option<String>,

    // ── Profile window ────────────────────────────────────────────────────
    /// Inclusive start of the profiled period (ISO 8601 date, e.g. `"2025-01-01"`).
    pub profiled_since: String,

    /// Inclusive end of the profiled period (ISO 8601 date, e.g. `"2026-05-31"`).
    pub profiled_until: String,

    /// Repositories included in this profile.
    pub repositories: Vec<String>,

    // ── Per-period data ───────────────────────────────────────────────────
    /// Period-level batches (statistics + sampled diffs), one per window.
    pub periods: Vec<PeriodBatch>,

    // ── Synthesised findings ──────────────────────────────────────────────
    /// All findings extracted across all periods, with trend annotations.
    pub all_findings: Vec<LongitudinalFinding>,

    /// Recurring strengths identified across periods (LLM-generated).
    pub strengths: Vec<String>,

    /// Recurring weaknesses / areas for improvement (LLM-generated).
    pub recurring_weaknesses: Vec<String>,

    // ── Trend summary ─────────────────────────────────────────────────────
    /// Overall quality trajectory direction.
    pub improvement_trajectory: Trajectory,

    /// Per-period quality scores as `(period_label, score)` pairs.
    /// Populated from `AuthorPeriodSummary::quality_score` across periods.
    pub quality_trend: Vec<(String, f64)>,

    // ── Narrative ─────────────────────────────────────────────────────────
    /// LLM-generated free-text narrative summarising the profile.
    /// Empty until the LLM narrative pass (Pass 2) populates it.
    pub narrative: String,

    // ── Telemetry ─────────────────────────────────────────────────────────
    /// Aggregate LLM token usage and cost for this profile run.
    pub token_cost: TokenCostSummary,

    // ── Metadata ──────────────────────────────────────────────────────────
    /// ISO 8601 UTC timestamp at which the profile was generated.
    pub generated_at: String,

    /// Pipeline version string — pinned to `"tr-profile-0.1"` for Pass 1.
    pub review_version: String,
}

impl ContributorProfile {
    /// Construct an empty `ContributorProfile` skeleton.
    ///
    /// Why: most fields are populated by downstream pipeline stages; a
    /// factory method avoids long constructor signatures and makes struct
    /// evolution backward-compatible.
    /// What: sets `review_version = PROFILE_VERSION`, `narrative = ""`,
    /// `generated_at` to the current UTC time (simple ISO format), and
    /// leaves slice fields empty.
    /// Test: exercised transitively by batch-assembly and selector tests.
    pub fn new(
        canonical_email: impl Into<String>,
        canonical_name: impl Into<String>,
        profiled_since: impl Into<String>,
        profiled_until: impl Into<String>,
    ) -> Self {
        Self {
            canonical_email: canonical_email.into(),
            canonical_name: canonical_name.into(),
            github_login: None,
            profiled_since: profiled_since.into(),
            profiled_until: profiled_until.into(),
            repositories: Vec::new(),
            periods: Vec::new(),
            all_findings: Vec::new(),
            strengths: Vec::new(),
            recurring_weaknesses: Vec::new(),
            improvement_trajectory: Trajectory::Stable,
            quality_trend: Vec::new(),
            narrative: String::new(),
            token_cost: TokenCostSummary::default(),
            generated_at: now_iso8601(),
            review_version: PROFILE_VERSION.to_string(),
        }
    }
}

/// Return a simple ISO-8601 UTC timestamp (`YYYY-MM-DDTHH:MM:SSZ`) using
/// only `std::time::SystemTime` — no chrono dep in the profile types module.
///
/// Why: `ContributorProfile` needs a timestamp but this module should not add
/// a direct chrono dep just for the timestamp field.
/// What: formats as `YYYY-MM-DDTHH:MM:SSZ` from epoch seconds.
/// Test: covered indirectly by `contributor_profile_serde_roundtrip`.
pub(crate) fn now_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86400;
    let (year, month, day) = epoch_days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Convert days since Unix epoch to `(year, month, day)`.
fn epoch_days_to_ymd(days: u64) -> (u64, u64, u64) {
    let z = days + 719468;
    let era = z / 146097;
    let doe = z % 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}
