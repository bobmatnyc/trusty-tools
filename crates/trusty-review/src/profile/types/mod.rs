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

pub mod core;
pub mod finding;
pub mod period;
pub mod token;

pub use core::{ContributorProfile, PROFILE_VERSION};
pub use finding::{LongitudinalFinding, TrendTag};
pub use period::{AuthorPeriodSummary, PeriodBatch, SampledDiff};
pub use token::{TokenCostSummary, Trajectory};

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

    /// Why: `TokenCostSummary::accumulate` must add values in-place.
    /// What: calls accumulate twice, asserts sum fields are correct.
    /// Test: this test itself.
    #[test]
    fn token_cost_summary_accumulate() {
        let mut tcs = TokenCostSummary::default();
        tcs.accumulate(100, 50, 0.001, 500);
        tcs.accumulate(200, 80, 0.002, 700);
        assert_eq!(tcs.input_tokens, 300);
        assert_eq!(tcs.output_tokens, 130);
        assert!((tcs.cost_usd - 0.003).abs() < 1e-10);
        assert_eq!(tcs.latency_ms, 1200);
    }
}
