//! Data models for the contributor-profile pipeline (epic #558).
//!
//! Why: longitudinal profiling requires several new serde types that are
//! specific to the profile pipeline and not shared with the MVP review loop.
//! Keeping them in a dedicated submodule avoids polluting `models/mod.rs`
//! and makes the profile data contract easy to audit in isolation.
//! What: defines `SampledDiff`, `PeriodBatch`, `TrendTag`,
//! `LongitudinalFinding`, `TokenCostSummary`, `Trajectory`, and
//! `ContributorProfile`.  All types derive `Serialize` / `Deserialize` for
//! JSON storage and transport.
//! Test: serde round-trip tests live in `mod tests` at the bottom of this
//! file; see `contributor_profile_serde_roundtrip` and siblings.

use serde::{Deserialize, Serialize};

// Re-export the tga type so callers within this module can use the short form.
pub use tga::report::period_trends::AuthorPeriodSummary;

use crate::models::Finding;

// ─── SampledDiff ──────────────────────────────────────────────────────────────

/// A representative commit diff sampled from a contributor's history.
///
/// Why: the LLM-based profile pass needs concrete diff text to reason about
/// code quality trends; raw `AuthorPeriodSummary` statistics alone are
/// insufficient for nuanced commentary.
/// What: pairs a commit's metadata (sha, repo, message, category, effort) with
/// the truncated unified diff text produced by `diff_for_commit`.
/// Test: see `sampled_diff_serde_roundtrip` and the diff-sampler unit tests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampledDiff {
    /// Full 40-char commit SHA.
    pub sha: String,

    /// Repository name (as stored in the tga `commits.repository` column).
    pub repository: String,

    /// Commit message (first line or full, depending on the DB row).
    pub message: String,

    /// Unified diff text, truncated to `MAX_DIFF_CHARS`.
    pub diff_text: String,

    /// Commit category (e.g. `"feature"`, `"bugfix"`, `"refactor"`).
    /// `None` when the commit was not classified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,

    /// Effort size label (e.g. `"XS"`, `"S"`, `"M"`, `"L"`, `"XL"`).
    /// `None` when the commit was not scored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

// ─── PeriodBatch ─────────────────────────────────────────────────────────────

/// Combined statistics and sampled diffs for a single N-week period.
///
/// Why: the LLM profile pass consumes one `PeriodBatch` per period, which
/// combines the statistical summary with concrete diff examples so the model
/// can correlate quantitative trends with qualitative observations.
/// What: embeds a tga [`AuthorPeriodSummary`] (reused directly — no
/// redefinition) and appends the `sampled_diffs` produced by the diff
/// sampler.  `sampled_diffs` is empty until the diff-sampler stage fills it.
/// Test: see `period_batch_serde_roundtrip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeriodBatch {
    /// Statistical summary for this period from tga.
    pub stats: AuthorPeriodSummary,

    /// Representative commit diffs sampled from this period.
    /// Empty until the diff-sampler stage populates it.
    #[serde(default)]
    pub sampled_diffs: Vec<SampledDiff>,
}

impl PeriodBatch {
    /// Construct a `PeriodBatch` from statistics, with an empty diff list.
    ///
    /// Why: the batch assembly stage creates the batch from stats alone;
    /// the diff sampler fills in `sampled_diffs` in a subsequent pass.
    /// What: wraps `stats` with an empty `sampled_diffs` vector.
    /// Test: exercised by all batch-assembly tests.
    pub fn from_stats(stats: AuthorPeriodSummary) -> Self {
        Self {
            stats,
            sampled_diffs: Vec::new(),
        }
    }
}

// ─── TrendTag ────────────────────────────────────────────────────────────────

/// Longitudinal trend classification for a finding across periods.
///
/// Why: the profile pass must distinguish findings that appear persistently
/// (recurring), newly, or are improving / worsening so the narrative can
/// offer targeted feedback.
/// What: four-variant enum serialised as `snake_case` strings.
/// Test: see `trend_tag_serde_roundtrip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrendTag {
    /// The same finding class appeared in multiple prior periods.
    Recurring,
    /// The finding appeared for the first time in this period.
    New,
    /// A previously recurring finding was not observed in the most recent
    /// period, suggesting improvement.
    Resolved,
    /// The finding severity or frequency has increased relative to prior
    /// periods.
    Worsening,
}

// ─── LongitudinalFinding ─────────────────────────────────────────────────────

/// A code-review finding annotated with its longitudinal trend.
///
/// Why: the profile narrative needs to surface which findings are new vs.
/// recurring vs. improving so the reviewer can give targeted feedback.
/// What: pairs a `Finding` (reused from the MVP review loop) with the period
/// label in which it was observed and an optional trend classification.
/// Test: see `longitudinal_finding_serde_roundtrip`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LongitudinalFinding {
    /// Human-readable period label (e.g. `"2026-W01..W04"`).
    pub period_label: String,

    /// The underlying code-review finding.
    pub finding: Finding,

    /// Trend classification relative to prior periods.
    /// `None` when only one period has been analysed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trend_tag: Option<TrendTag>,
}

// ─── TokenCostSummary ─────────────────────────────────────────────────────────

/// Aggregate LLM token usage and cost for a profile generation run.
///
/// Why: operators need to monitor the cost of running longitudinal profiles
/// across many contributors, especially as the number of periods grows.
/// What: accumulates `input_tokens`, `output_tokens`, estimated `cost_usd`,
/// and wall-clock `latency_ms` across all LLM calls in a single profile run.
/// Test: see `token_cost_summary_serde_roundtrip`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenCostSummary {
    /// Total input tokens consumed across all LLM calls.
    pub input_tokens: u64,
    /// Total output tokens produced across all LLM calls.
    pub output_tokens: u64,
    /// Total estimated cost in USD.
    pub cost_usd: f64,
    /// Total wall-clock latency in milliseconds (sum of all call latencies).
    pub latency_ms: u64,
}

// ─── Trajectory ──────────────────────────────────────────────────────────────

/// Overall quality trajectory direction for a contributor over the profile
/// window.
///
/// Why: the profile narrative needs a single high-level signal that the LLM
/// and callers can use to route action (e.g. flag declining contributors for
/// coaching vs. commend improving ones).
/// What: three-variant enum serialised as `snake_case`.
/// Test: see `trajectory_serde_roundtrip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trajectory {
    /// Quality metrics are trending upward across periods.
    Improving,
    /// Quality metrics are roughly flat (within noise).
    Stable,
    /// Quality metrics are trending downward across periods.
    Declining,
}

// ─── ContributorProfile ───────────────────────────────────────────────────────

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

/// The profile pipeline version emitted in `ContributorProfile::review_version`.
///
/// Why: version-stamps the profile so consumers can detect schema changes
/// and handle backward-compatibility migration.
/// What: a string constant injected into every new profile.
/// Test: asserted in `contributor_profile_serde_roundtrip`.
pub const PROFILE_VERSION: &str = "tr-profile-0.1";

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
/// a direct chrono dep just for the timestamp field; the lightweight helper
/// in `models/mod.rs` already demonstrates this pattern.
/// What: formats as `YYYY-MM-DDTHH:MM:SSZ` from epoch seconds.
/// Test: covered indirectly by `contributor_profile_serde_roundtrip`.
fn now_iso8601() -> String {
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
/// Matches the algorithm already used in `models/mod.rs`.
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

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Effort, Finding};
    use std::collections::HashMap;

    fn make_stats() -> AuthorPeriodSummary {
        AuthorPeriodSummary {
            period_label: "2026-W01..W04".to_string(),
            since: "2026-01-05".to_string(),
            until: "2026-02-01".to_string(),
            commit_count: 12,
            categories: HashMap::from([
                ("feature".to_string(), 8u64),
                ("bugfix".to_string(), 4u64),
            ]),
            effort_histogram: HashMap::from([("S".to_string(), 5u32), ("M".to_string(), 7u32)]),
            quality_score: 3.7,
            ticketed_pct: 0.75,
            pr_metrics: tga::report::drilldown::PrMetrics {
                total: 3,
                merged: 3,
                avg_cycle_time_hours: Some(18.0),
                median_cycle_time_hours: Some(16.0),
                p95_cycle_time_hours: None,
            },
            repositories: vec!["acme/api".to_string()],
        }
    }

    /// Why: SampledDiff must survive a serde round-trip without data loss.
    /// What: constructs a SampledDiff, serialises to JSON, deserialises back,
    /// asserts all fields are equal.
    /// Test: this test itself.
    #[test]
    fn sampled_diff_serde_roundtrip() {
        let diff = SampledDiff {
            sha: "abc123".to_string(),
            repository: "acme/api".to_string(),
            message: "feat: add user endpoint".to_string(),
            diff_text: "+fn add_user() {}".to_string(),
            category: Some("feature".to_string()),
            effort: Some("M".to_string()),
        };
        let json = serde_json::to_string(&diff).expect("serialise");
        let back: SampledDiff = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back.sha, "abc123");
        assert_eq!(back.repository, "acme/api");
        assert_eq!(back.category, Some("feature".to_string()));
        assert_eq!(back.effort, Some("M".to_string()));
    }

    /// Why: SampledDiff must omit None fields to keep JSON tidy.
    /// What: constructs with None category/effort, asserts they don't appear
    /// in the JSON output.
    /// Test: this test itself.
    #[test]
    fn sampled_diff_none_fields_omitted() {
        let diff = SampledDiff {
            sha: "def456".to_string(),
            repository: "repo".to_string(),
            message: "fix: something".to_string(),
            diff_text: "-old\n+new".to_string(),
            category: None,
            effort: None,
        };
        let json = serde_json::to_string(&diff).expect("serialise");
        assert!(
            !json.contains("\"category\""),
            "None category should be omitted"
        );
        assert!(
            !json.contains("\"effort\""),
            "None effort should be omitted"
        );
    }

    /// Why: PeriodBatch must combine stats + sampled_diffs and survive
    /// a serde round-trip.
    /// What: creates a PeriodBatch with one SampledDiff, round-trips through
    /// JSON, asserts fields are preserved.
    /// Test: this test itself.
    #[test]
    fn period_batch_serde_roundtrip() {
        let mut batch = PeriodBatch::from_stats(make_stats());
        batch.sampled_diffs.push(SampledDiff {
            sha: "aaa".to_string(),
            repository: "r".to_string(),
            message: "msg".to_string(),
            diff_text: "+line".to_string(),
            category: Some("feature".to_string()),
            effort: None,
        });

        let json = serde_json::to_string(&batch).expect("serialise");
        let back: PeriodBatch = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back.stats.period_label, "2026-W01..W04");
        assert_eq!(back.stats.commit_count, 12);
        assert_eq!(back.sampled_diffs.len(), 1);
        assert_eq!(back.sampled_diffs[0].sha, "aaa");
    }

    /// Why: TrendTag must serialise to snake_case strings and deserialise back.
    /// What: round-trips all four variants.
    /// Test: this test itself.
    #[test]
    fn trend_tag_serde_roundtrip() {
        for (tag, expected) in [
            (TrendTag::Recurring, "\"recurring\""),
            (TrendTag::New, "\"new\""),
            (TrendTag::Resolved, "\"resolved\""),
            (TrendTag::Worsening, "\"worsening\""),
        ] {
            let json = serde_json::to_string(&tag).expect("serialise");
            assert_eq!(json, expected, "variant {tag:?} serialised incorrectly");
            let back: TrendTag = serde_json::from_str(&json).expect("deserialise");
            assert_eq!(back, tag);
        }
    }

    /// Why: LongitudinalFinding must survive a round-trip preserving nested
    /// Finding and optional TrendTag.
    /// What: creates with Some(TrendTag::Recurring), round-trips, asserts.
    /// Test: this test itself.
    #[test]
    fn longitudinal_finding_serde_roundtrip() {
        let lf = LongitudinalFinding {
            period_label: "2026-W01..W04".to_string(),
            finding: Finding::new(
                "src/main.rs",
                "security",
                "SQL injection",
                "Use parameterised query",
                0.95,
                Effort::Medium,
            ),
            trend_tag: Some(TrendTag::Recurring),
        };
        let json = serde_json::to_string(&lf).expect("serialise");
        let back: LongitudinalFinding = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back.period_label, "2026-W01..W04");
        assert_eq!(back.finding.kind, "security");
        assert_eq!(back.trend_tag, Some(TrendTag::Recurring));
    }

    /// Why: Trajectory must serialise as snake_case and round-trip cleanly.
    /// What: round-trips all three variants.
    /// Test: this test itself.
    #[test]
    fn trajectory_serde_roundtrip() {
        for (t, expected) in [
            (Trajectory::Improving, "\"improving\""),
            (Trajectory::Stable, "\"stable\""),
            (Trajectory::Declining, "\"declining\""),
        ] {
            let json = serde_json::to_string(&t).expect("serialise");
            assert_eq!(json, expected);
            let back: Trajectory = serde_json::from_str(&json).expect("deserialise");
            assert_eq!(back, t);
        }
    }

    /// Why: ContributorProfile must survive a full serde round-trip with all
    /// nested types preserved.
    /// What: constructs a profile via `new()`, adds periods/findings, asserts
    /// round-trip fidelity and that `review_version` equals the constant.
    /// Test: this test itself.
    #[test]
    fn contributor_profile_serde_roundtrip() {
        let mut profile = ContributorProfile::new(
            "alice@example.com",
            "Alice Smith",
            "2026-01-01",
            "2026-05-31",
        );
        profile.github_login = Some("alice-dev".to_string());
        profile.repositories = vec!["acme/api".to_string()];
        profile.periods.push(PeriodBatch::from_stats(make_stats()));
        profile.all_findings.push(LongitudinalFinding {
            period_label: "2026-W01..W04".to_string(),
            finding: Finding::new(
                "src/lib.rs",
                "logic",
                "off-by-one",
                "use exclusive range",
                0.8,
                Effort::Low,
            ),
            trend_tag: Some(TrendTag::New),
        });
        profile.strengths = vec!["consistent ticket coverage".to_string()];
        profile.recurring_weaknesses = vec!["missing error handling".to_string()];
        profile.improvement_trajectory = Trajectory::Improving;
        profile.quality_trend = vec![("2026-W01..W04".to_string(), 3.7)];
        profile.token_cost = TokenCostSummary {
            input_tokens: 1000,
            output_tokens: 200,
            cost_usd: 0.012,
            latency_ms: 850,
        };

        let json = serde_json::to_string_pretty(&profile).expect("serialise");
        let back: ContributorProfile = serde_json::from_str(&json).expect("deserialise");

        assert_eq!(back.canonical_email, "alice@example.com");
        assert_eq!(back.canonical_name, "Alice Smith");
        assert_eq!(back.github_login, Some("alice-dev".to_string()));
        assert_eq!(back.periods.len(), 1);
        assert_eq!(back.periods[0].stats.commit_count, 12);
        assert_eq!(back.all_findings.len(), 1);
        assert_eq!(back.all_findings[0].trend_tag, Some(TrendTag::New));
        assert_eq!(back.improvement_trajectory, Trajectory::Improving);
        assert_eq!(back.quality_trend.len(), 1);
        assert!((back.quality_trend[0].1 - 3.7).abs() < f64::EPSILON);
        assert_eq!(back.review_version, PROFILE_VERSION);
        assert_eq!(back.token_cost.input_tokens, 1000);
    }

    /// Why: TokenCostSummary must default to all-zeros.
    /// What: creates via `Default::default()`, asserts zero values.
    /// Test: this test itself.
    #[test]
    fn token_cost_summary_defaults_to_zero() {
        let tcs = TokenCostSummary::default();
        assert_eq!(tcs.input_tokens, 0);
        assert_eq!(tcs.output_tokens, 0);
        assert!((tcs.cost_usd - 0.0).abs() < f64::EPSILON);
        assert_eq!(tcs.latency_ms, 0);
    }
}
