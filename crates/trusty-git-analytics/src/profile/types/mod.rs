//! Data models for the contributor-profile pipeline.
//!
//! Why: profiling introduces several serde types that no other tga stage uses;
//! keeping them in one submodule makes the profile's data contract auditable in
//! isolation and keeps `core::models` free of them.
//! What: re-exports [`ContributorProfile`], [`PeriodBatch`], [`SampledDiff`],
//! [`ProfileFinding`], [`LongitudinalFinding`], [`TrendTag`],
//! [`TokenCostSummary`], and [`Trajectory`] from their per-type submodules.
//! Every type round-trips through JSON.
//! Test: the `tests` module at the bottom of this file.

pub mod core;
pub mod finding;
pub mod period;
pub mod token;

pub use core::{ContributorProfile, PROFILE_VERSION};
pub use finding::{FindingEffort, LongitudinalFinding, ProfileFinding, TrendTag};
pub use period::{AuthorPeriodSummary, PeriodBatch, SampledDiff};
pub use token::{TokenCostSummary, Trajectory};

// ─── Unit tests ──────────────────────────────────────────────────────────────

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

    /// Why: a sampled diff is written to `profile.json` and read back by the
    /// renderer, so a lossy round-trip would silently drop diff evidence.
    /// What: serialises a fully-populated `SampledDiff` and asserts every field
    /// survives deserialisation.
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

    /// Why: unclassified commits are common, and writing `"category": null` for
    /// every one of them bloats the profile JSON.
    /// What: serialises a `SampledDiff` with both optional fields `None` and
    /// asserts neither key appears.
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
        assert!(!json.contains("\"category\""), "None category is omitted");
        assert!(!json.contains("\"effort\""), "None effort is omitted");
    }

    /// Why: `PeriodBatch` embeds tga's own `AuthorPeriodSummary`; a round-trip
    /// proves that embedding stays serialisable as the summary type evolves.
    /// What: builds a batch with one sampled diff, round-trips it, and asserts
    /// both halves survive.
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

    /// Why: the trend tag is the profile's headline signal, so its wire form is
    /// part of the data contract, not an implementation detail.
    /// What: asserts each variant serialises to its exact snake_case string and
    /// reads back as the same variant.
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

    /// Why: `ProfileFinding` exists to read JSON produced by an external review
    /// pass; if the extra review-only fields broke deserialisation, the whole
    /// DTO would be useless (#5463).
    /// What: parses a finding payload carrying the review-side extras
    /// (`suggested_replacement`, `category`, `code_provable`, `verified`,
    /// `issue_eligible`) and asserts the shared fields land correctly.
    /// Test: this test itself.
    #[test]
    fn profile_finding_parses_review_json() {
        let review_json = r#"{
            "file": "src/main.rs",
            "line": 42,
            "kind": "security",
            "description": "SQL injection",
            "consequence": "attacker reads the whole table",
            "suggestion": "use a parameterised query",
            "suggested_replacement": "conn.execute(sql, params![id])",
            "confidence": 0.95,
            "effort": "medium",
            "category": "correctness",
            "source_citation": "SPEC-1 §2",
            "code_provable": true,
            "issue_eligible": false
        }"#;
        let f: ProfileFinding = serde_json::from_str(review_json).expect("parse review finding");
        assert_eq!(f.file, "src/main.rs");
        assert_eq!(f.line, Some(42));
        assert_eq!(f.kind, "security");
        assert_eq!(f.consequence, "attacker reads the whole table");
        assert_eq!(f.effort, FindingEffort::Medium);
        assert!((f.confidence - 0.95).abs() < 1e-6);
    }

    /// Why: model-produced confidence values are routinely out of range, and an
    /// unclamped value would distort the worsening comparison in trend tagging.
    /// What: constructs findings with confidence above 1.0 and below 0.0 and
    /// asserts both are clamped.
    /// Test: this test itself.
    #[test]
    fn profile_finding_clamps_confidence() {
        let high = ProfileFinding::new("f", "k", "d", "s", 4.2, FindingEffort::Low);
        assert!((high.confidence - 1.0).abs() < f32::EPSILON);
        let low = ProfileFinding::new("f", "k", "d", "s", -1.0, FindingEffort::High);
        assert!((low.confidence - 0.0).abs() < f32::EPSILON);
        assert_eq!(low.effort.to_string(), "high");
    }

    /// Why: the finding plus its period label and trend tag is what the report
    /// renders, so the nesting has to survive a round-trip intact.
    /// What: round-trips a `LongitudinalFinding` carrying a `TrendTag`.
    /// Test: this test itself.
    #[test]
    fn longitudinal_finding_serde_roundtrip() {
        let lf = LongitudinalFinding {
            period_label: "2026-W01..W04".to_string(),
            finding: ProfileFinding::new(
                "src/main.rs",
                "security",
                "SQL injection",
                "Use parameterised query",
                0.95,
                FindingEffort::Medium,
            ),
            trend_tag: Some(TrendTag::Recurring),
        };
        let json = serde_json::to_string(&lf).expect("serialise");
        let back: LongitudinalFinding = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back.period_label, "2026-W01..W04");
        assert_eq!(back.finding.kind, "security");
        assert_eq!(back.trend_tag, Some(TrendTag::Recurring));
    }

    /// Why: the trajectory string is read by dashboards, so its wire form is
    /// part of the contract.
    /// What: round-trips all three variants against their exact JSON strings.
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

    /// Why: the profile is written to disk and read back by later runs and by
    /// the renderer; a lossy round-trip anywhere in the nested tree would lose
    /// evidence silently.
    /// What: populates every field, round-trips through pretty JSON, and
    /// asserts identity, periods, findings, trajectory, version, and telemetry.
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
            finding: ProfileFinding::new(
                "src/lib.rs",
                "logic",
                "off-by-one",
                "use exclusive range",
                0.8,
                FindingEffort::Low,
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

    /// Why: a fresh summary must read as "no model call was made", which is what
    /// the renderer keys the cost section off.
    /// What: asserts every field of `TokenCostSummary::default()` is zero.
    /// Test: this test itself.
    #[test]
    fn token_cost_summary_defaults_to_zero() {
        let tcs = TokenCostSummary::default();
        assert_eq!(tcs.input_tokens, 0);
        assert_eq!(tcs.output_tokens, 0);
        assert!((tcs.cost_usd - 0.0).abs() < f64::EPSILON);
        assert_eq!(tcs.latency_ms, 0);
    }

    /// Why: the summary is the sum over many calls, so accumulation must be
    /// additive in every field including the float cost.
    /// What: accumulates twice and asserts each field is the sum.
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
