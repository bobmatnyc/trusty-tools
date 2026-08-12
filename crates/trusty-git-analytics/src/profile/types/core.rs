//! The top-level [`ContributorProfile`] artefact.
//!
//! Why: the profile is written to disk and read back by later runs, dashboards,
//! and (from #5465) a GitHub issue thread, so it has to be self-contained and
//! version-stamped rather than reconstructible only from the pipeline that
//! produced it.
//! What: defines [`ContributorProfile`] and [`PROFILE_VERSION`].
//! Test: `contributor_profile_serde_roundtrip` in the parent `types` test module.

use serde::{Deserialize, Serialize};

use super::finding::LongitudinalFinding;
use super::period::PeriodBatch;
use super::token::{TokenCostSummary, Trajectory};

/// Schema version stamped into every [`ContributorProfile`].
///
/// Why: a stored profile outlives the binary that wrote it, so a reader needs to
/// know which field set to expect. This is deliberately not the crate version —
/// most tga releases do not change the profile schema.
/// What: `"tga-profile-0.1"`. Distinct from trusty-review's `"tr-profile-0.1"`
/// because #5463 gave the DTOs a tga-native finding shape, so the two are not
/// interchangeable on the wire.
/// Test: asserted in `contributor_profile_serde_roundtrip`.
pub const PROFILE_VERSION: &str = "tga-profile-0.1";

/// A contributor's longitudinal quality profile over one window.
///
/// Why: everything a reader needs — identity, window, per-period data, findings,
/// trajectory, narrative, and what the run cost — travels in one serialisable
/// object, so consuming a profile never means re-querying the database.
/// What: identity and window metadata, the [`PeriodBatch`] list, the tagged
/// [`LongitudinalFinding`] list, the strengths/weaknesses the narrative pass
/// fills in, a [`Trajectory`], and telemetry.
/// Test: `contributor_profile_serde_roundtrip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContributorProfile {
    // ── Identity ──────────────────────────────────────────────────────────
    /// Canonical email, as stored in `authors.canonical_email`.
    pub canonical_email: String,

    /// Canonical display name, as stored in `authors.canonical_name`.
    pub canonical_name: String,

    /// GitHub login, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub github_login: Option<String>,

    // ── Profile window ────────────────────────────────────────────────────
    /// Inclusive start of the profiled window (ISO 8601 date).
    pub profiled_since: String,

    /// Inclusive end of the profiled window (ISO 8601 date).
    pub profiled_until: String,

    /// Repositories covered by this profile.
    pub repositories: Vec<String>,

    // ── Per-period data ───────────────────────────────────────────────────
    /// One batch per window, in chronological order.
    pub periods: Vec<PeriodBatch>,

    // ── Synthesised findings ──────────────────────────────────────────────
    /// Every finding across every period, trend-tagged.
    pub all_findings: Vec<LongitudinalFinding>,

    /// Recurring strengths; populated by the narrative pass.
    pub strengths: Vec<String>,

    /// Recurring weaknesses; populated by the narrative pass.
    pub recurring_weaknesses: Vec<String>,

    // ── Trend summary ─────────────────────────────────────────────────────
    /// Overall quality direction.
    pub improvement_trajectory: Trajectory,

    /// Per-period quality scores as `(period_label, score)`.
    pub quality_trend: Vec<(String, f64)>,

    // ── Narrative ─────────────────────────────────────────────────────────
    /// Free-text assessment. Empty until a narrative pass fills it.
    pub narrative: String,

    // ── Telemetry ─────────────────────────────────────────────────────────
    /// LLM token usage and cost for this run.
    pub token_cost: TokenCostSummary,

    // ── Metadata ──────────────────────────────────────────────────────────
    /// UTC timestamp at which the profile was generated (ISO 8601).
    pub generated_at: String,

    /// Schema version; see [`PROFILE_VERSION`].
    pub review_version: String,
}

impl ContributorProfile {
    /// Build an empty profile skeleton for a contributor and window.
    ///
    /// Why: the pipeline fills most fields in stages, so a constructor taking
    /// all of them would have to be rewritten every time a stage is added.
    /// What: sets identity and window, stamps [`PROFILE_VERSION`] and the
    /// current UTC time, and leaves every collection empty.
    /// Test: exercised by `contributor_profile_serde_roundtrip` and every
    /// reporter and synthesizer test.
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
            // trusty-review's hand-rolled civil-from-days conversion, which
            // existed only to keep `profile/types/` chrono-free.
            generated_at: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            review_version: PROFILE_VERSION.to_string(),
        }
    }
}
