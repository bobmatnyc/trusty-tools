//! The top-level [`ContributorProfile`] and its version stamp.
//!
//! Why: the profile is the pipeline's whole output — identity, per-period data,
//! trend-tagged findings, narrative, and telemetry in one serialisable value
//! that can be written to disk, posted to a tracker, or fed to a dashboard.
//! What: defines [`ContributorProfile`] and [`PROFILE_VERSION`].
//! Test: `contributor_profile_serde_roundtrip` in the parent `tests` module.

use serde::{Deserialize, Serialize};

use super::finding::LongitudinalFinding;
use super::period::PeriodBatch;
use super::token::{TokenCostSummary, Trajectory};

/// Schema version stamped into every [`ContributorProfile`].
///
/// Why: a consumer reading an old `profile.json` has to be able to tell which
/// schema produced it. The `tga-` prefix distinguishes tga-produced profiles
/// from the `tr-profile-0.1` files trusty-review emitted before profiling moved
/// here (#5463, epic #5468).
/// What: a string constant written to `ContributorProfile::review_version`.
/// Test: asserted in `contributor_profile_serde_roundtrip`.
pub const PROFILE_VERSION: &str = "tga-profile-0.1";

/// A full longitudinal contributor profile.
///
/// Why: the profile has to stand alone — a reader who has only the JSON must
/// be able to reconstruct who it describes, over what window, and on what
/// evidence, without re-querying the database it came from.
/// What: aggregates identity, the profile window, per-period batches,
/// trend-tagged findings, strengths/weaknesses, a trajectory, a narrative, and
/// token telemetry. `#[non_exhaustive]` so later passes can add fields without
/// a major bump; construct it with [`ContributorProfile::new`].
/// Test: `contributor_profile_serde_roundtrip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ContributorProfile {
    // ── Identity ──────────────────────────────────────────────────────────
    /// Canonical email, as stored in `authors.canonical_email`.
    pub canonical_email: String,

    /// Canonical display name, as stored in `authors.canonical_name`.
    pub canonical_name: String,

    /// GitHub login, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_login: Option<String>,

    // ── Profile window ────────────────────────────────────────────────────
    /// Inclusive start of the profiled window (ISO 8601 date).
    pub profiled_since: String,

    /// Inclusive end of the profiled window (ISO 8601 date).
    pub profiled_until: String,

    /// Repositories included in this profile.
    pub repositories: Vec<String>,

    // ── Per-period data ───────────────────────────────────────────────────
    /// Period-level batches (statistics plus sampled diffs), one per window.
    pub periods: Vec<PeriodBatch>,

    // ── Synthesised findings ──────────────────────────────────────────────
    /// Every finding across every period, annotated with its trend.
    pub all_findings: Vec<LongitudinalFinding>,

    /// Recurring strengths identified across periods.
    pub strengths: Vec<String>,

    /// Recurring weaknesses / areas for improvement.
    pub recurring_weaknesses: Vec<String>,

    // ── Trend summary ─────────────────────────────────────────────────────
    /// Overall quality-trajectory direction.
    pub improvement_trajectory: Trajectory,

    /// Per-period quality scores as `(period_label, score)` pairs.
    pub quality_trend: Vec<(String, f64)>,

    // ── Narrative ─────────────────────────────────────────────────────────
    /// Free-text assessment of the profile. The deterministic pass writes a
    /// factual summary here; the narrative pass (#5464) replaces it.
    pub narrative: String,

    // ── Telemetry ─────────────────────────────────────────────────────────
    /// Aggregate model token usage and cost for this run. All-zero when the
    /// run was purely deterministic.
    pub token_cost: TokenCostSummary,

    // ── Metadata ──────────────────────────────────────────────────────────
    /// ISO 8601 UTC timestamp at which the profile was generated.
    pub generated_at: String,

    /// Schema version — always [`PROFILE_VERSION`] for a freshly built profile.
    pub review_version: String,
}

impl ContributorProfile {
    /// Construct an empty profile skeleton for a contributor and window.
    ///
    /// Why: every later stage fills in a different part of the profile, so it
    /// has to be constructible from identity and window alone.
    /// What: stamps `review_version` with [`PROFILE_VERSION`] and
    /// `generated_at` with the current UTC time; every collection starts empty
    /// and the trajectory starts at [`Trajectory::Stable`].
    /// Test: `contributor_profile_serde_roundtrip`.
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
            // #5463: tga already depends on chrono, so the port drops
            // trusty-review's hand-rolled civil-date arithmetic here.
            generated_at: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            review_version: PROFILE_VERSION.to_string(),
        }
    }
}
