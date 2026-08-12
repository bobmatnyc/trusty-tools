//! Data model for the contributor-profile pipeline.
//!
//! Why: the profile artefact is written to disk and read back later, so its
//! types are a stored contract, not internal plumbing. Keeping them in one
//! submodule makes that contract auditable in isolation from the pipeline
//! stages that populate it.
//! What: re-exports [`ContributorProfile`], [`PeriodBatch`], [`SampledDiff`],
//! [`Finding`], [`LongitudinalFinding`], [`TokenCostSummary`], [`Trajectory`],
//! [`TrendTag`], [`Effort`], and [`PROFILE_VERSION`].
//! Test: the serde round-trips in the `tests` module at the bottom of this file.

pub mod core;
pub mod finding;
pub mod period;
pub mod token;

pub use core::{ContributorProfile, PROFILE_VERSION};
pub use finding::{Effort, Finding, LongitudinalFinding, TrendTag};
pub use period::{AuthorPeriodSummary, PeriodBatch, SampledDiff};
pub use token::{TokenCostSummary, Trajectory};

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
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
            pr_metrics: crate::report::drilldown::PrMetrics {
                total: 3,
                merged: 3,
                avg_cycle_time_hours: Some(18.0),
                median_cycle_time_hours: Some(16.0),
                p95_cycle_time_hours: None,
            },
            repositories: vec!["acme/api".to_string()],
        }
    }

    /// Why: `SampledDiff` must survive a serde round-trip without data loss.
    /// What: constructs one, serialises to JSON, deserialises back, asserts fields.
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

    /// Why: `None` fields must be omitted so the stored JSON stays tidy.
    /// What: constructs with `None` category/effort, asserts neither key appears.
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

    /// Why: `PeriodBatch` must carry stats and diffs through a round-trip.
    /// What: builds a batch with one diff, round-trips, asserts both survive.
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

    /// Why: `TrendTag` must serialise to snake_case and deserialise back.
    /// What: round-trips all four variants, asserting the exact JSON string.
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

    /// Why: `Effort` must serialise lowercase so stored profiles stay readable.
    /// What: round-trips all three variants, asserting the exact JSON string.
    /// Test: this test itself.
    #[test]
    fn effort_serde_roundtrip() {
        for (effort, expected) in [
            (Effort::Low, "\"low\""),
            (Effort::Medium, "\"medium\""),
            (Effort::High, "\"high\""),
        ] {
            let json = serde_json::to_string(&effort).expect("serialise");
            assert_eq!(json, expected);
            let back: Effort = serde_json::from_str(&json).expect("deserialise");
            assert_eq!(back, effort);
        }
    }

    /// Why: confidence originates in model output, so an out-of-range value is
    /// expected; it must be clamped rather than stored and later skewing the
    /// severity comparison in `assign_trend_tags`.
    /// What: constructs findings with 1.5 and -0.5, asserts 1.0 and 0.0.
    /// Test: this test itself.
    #[test]
    fn finding_confidence_is_clamped() {
        let high = Finding::new("f.rs", "logic", "d", "s", 1.5, Effort::Low);
        assert!((high.confidence - 1.0).abs() < f32::EPSILON);
        let low = Finding::new("f.rs", "logic", "d", "s", -0.5, Effort::Low);
        assert!((low.confidence - 0.0).abs() < f32::EPSILON);
    }

    /// Why: `LongitudinalFinding` must preserve the nested finding and tag.
    /// What: builds one with `Some(Recurring)`, round-trips, asserts fields.
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
        assert_eq!(back.finding.effort, Effort::Medium);
        assert_eq!(back.trend_tag, Some(TrendTag::Recurring));
    }

    /// Why: `Trajectory` must serialise as snake_case and round-trip cleanly.
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

    /// Why: the whole profile is a stored artefact, so a full round-trip with
    /// every nested type populated is the contract this module owes.
    /// What: builds a profile via `new()`, fills every field, round-trips
    /// through pretty JSON, asserts fidelity and the version stamp.
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

    /// Why: `TokenCostSummary` must default to all-zeros so a deterministic-only
    /// run reports no spend rather than uninitialised numbers.
    /// What: builds via `Default::default()`, asserts every field is zero.
    /// Test: this test itself.
    #[test]
    fn token_cost_summary_defaults_to_zero() {
        let tcs = TokenCostSummary::default();
        assert_eq!(tcs.input_tokens, 0);
        assert_eq!(tcs.output_tokens, 0);
        assert!((tcs.cost_usd - 0.0).abs() < f64::EPSILON);
        assert_eq!(tcs.latency_ms, 0);
    }

    /// Why: `accumulate` must sum in place across calls.
    /// What: calls it twice, asserts each field is the sum.
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
