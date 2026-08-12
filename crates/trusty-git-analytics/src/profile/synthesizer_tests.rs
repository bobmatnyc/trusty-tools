//! Tests for cross-period synthesis.
//!
//! Why: split out of `synthesizer.rs` so that file stays well under the
//! production SLOC cap while the coverage stays whole.
//! What: covers Jaccard similarity, all four trend tags, trajectory slope,
//! deterministic synthesis, the narrative parse paths, and the fallback.
//! Test: included as `#[cfg(test)] mod tests` from `synthesizer.rs`.
//!
//! Two prompt tests from the trusty-review original are deliberately absent:
//! the `bedrock/` and `openrouter/` model-prefix-stripping regressions belong
//! with the provider routing they guard, which lands in #5464.

use std::collections::HashMap;

use super::{
    apply_deterministic_synthesis, apply_fallback_narrative, apply_synthesis_json,
    assign_trend_tags, build_synthesizer_user_message, derive_trajectory, jaccard_similarity,
    synthesis_output_schema, synthesizer_system_prompt,
};
use crate::profile::types::{
    AuthorPeriodSummary, ContributorProfile, Effort, Finding, LongitudinalFinding, PeriodBatch,
    Trajectory, TrendTag,
};

// ── Helpers ───────────────────────────────────────────────────────────────────

fn make_finding(period: &str, description: &str) -> LongitudinalFinding {
    make_finding_with_confidence(period, description, 0.8)
}

fn make_finding_with_confidence(
    period: &str,
    description: &str,
    confidence: f32,
) -> LongitudinalFinding {
    LongitudinalFinding {
        period_label: period.to_string(),
        finding: Finding::new(
            "src/lib.rs",
            "error_handling",
            description,
            "fix it",
            confidence,
            Effort::Medium,
        ),
        trend_tag: None,
    }
}

fn make_profile() -> ContributorProfile {
    ContributorProfile::new("alice@example.com", "Alice", "2026-01-01", "2026-12-31")
}

fn make_period(label: &str, score: f64) -> PeriodBatch {
    PeriodBatch::from_stats(AuthorPeriodSummary {
        period_label: label.to_string(),
        since: "2026-01-01".to_string(),
        until: "2026-03-31".to_string(),
        commit_count: 3,
        categories: HashMap::new(),
        effort_histogram: HashMap::new(),
        quality_score: score,
        ticketed_pct: 0.5,
        pr_metrics: crate::report::drilldown::PrMetrics {
            total: 1,
            merged: 1,
            avg_cycle_time_hours: None,
            median_cycle_time_hours: None,
            p95_cycle_time_hours: None,
        },
        repositories: vec!["acme/api".to_string()],
    })
}

// ── Jaccard ───────────────────────────────────────────────────────────────────

/// Why: the similarity metric decides which findings cluster, so its identity,
/// disjoint, and empty-input cases must all be pinned.
/// What: asserts 1.0 for identical strings, below 0.5 for disjoint ones, 1.0 for
/// two empties, and 0.0 when only one side is empty.
/// Test: this test itself.
#[test]
fn jaccard_similarity_basic() {
    assert!(
        (jaccard_similarity("error handling in async", "error handling in async") - 1.0).abs()
            < 1e-10
    );
    assert!(jaccard_similarity("error handling", "completely different concept") < 0.5);
    assert!((jaccard_similarity("", "") - 1.0).abs() < 1e-10);
    assert!((jaccard_similarity("foo", "") - 0.0).abs() < 1e-10);
}

/// Why: two wordings of the same recurring problem must score high enough to
/// cluster, or no trend ever surfaces.
/// What: scores two overlapping descriptions, asserts at least 0.6.
/// Test: this test itself.
#[test]
fn jaccard_similarity_similar_descriptions() {
    let a = "missing error propagation in async function";
    let b = "missing error propagation async function handler";
    let sim = jaccard_similarity(a, b);
    assert!(sim >= 0.6, "similar descriptions: sim={sim:.3}");
}

// ── Trend tag assignment ──────────────────────────────────────────────────────

/// Why: an issue present in the latest period and an earlier one is the case the
/// whole feature exists to surface.
/// What: tags two near-identical findings in different periods, asserts both are
/// `Recurring`.
/// Test: this test itself.
#[test]
fn synthesizer_dedup_assigns_recurring() {
    let findings = vec![
        make_finding("2026-Q1", "missing error propagation in async handler"),
        make_finding("2026-Q2", "missing error propagation in async handler"),
    ];
    let tagged = assign_trend_tags(findings);
    assert_eq!(tagged.len(), 2);
    for f in &tagged {
        assert_eq!(
            f.trend_tag,
            Some(TrendTag::Recurring),
            "both should be Recurring: {:?}",
            f.trend_tag
        );
    }
}

/// Why: an issue seen only in the latest period is new, not recurring.
/// What: tags a single latest-period finding, asserts `New`.
/// Test: this test itself.
#[test]
fn synthesizer_dedup_assigns_new() {
    let findings = vec![make_finding(
        "2026-Q2",
        "newly introduced SQL injection risk",
    )];
    let tagged = assign_trend_tags(findings);
    assert_eq!(tagged[0].trend_tag, Some(TrendTag::New));
}

/// Why: an issue absent from the latest period is the good news a profile owes
/// the reader, and must not be reported as still open.
/// What: puts one finding in Q1 and an unrelated one in Q2, asserts the Q1
/// cluster is `Resolved`.
/// Test: this test itself.
#[test]
fn synthesizer_dedup_assigns_resolved() {
    let findings = vec![
        make_finding("2026-Q1", "unreachable panic in fallback path"),
        make_finding("2026-Q2", "completely unrelated memory allocation issue"),
    ];
    let tagged = assign_trend_tags(findings);
    let q1 = tagged
        .iter()
        .find(|f| f.period_label == "2026-Q1")
        .expect("Q1 finding present");
    assert_eq!(
        q1.trend_tag,
        Some(TrendTag::Resolved),
        "Q1-only finding must be Resolved"
    );
}

/// Why: a recurring issue whose confidence climbed is worse news than one
/// holding steady, and the tag is the only thing that distinguishes them.
/// What: clusters the same description across two periods with confidence
/// rising 0.6 → 0.9, asserts `Worsening` rather than `Recurring`.
/// Test: this test itself.
#[test]
fn synthesizer_dedup_assigns_worsening() {
    let findings = vec![
        make_finding_with_confidence("2026-Q1", "unchecked index into the buffer", 0.6),
        make_finding_with_confidence("2026-Q2", "unchecked index into the buffer", 0.9),
    ];
    let tagged = assign_trend_tags(findings);
    for f in &tagged {
        assert_eq!(
            f.trend_tag,
            Some(TrendTag::Worsening),
            "rising confidence must tag Worsening, got {:?}",
            f.trend_tag
        );
    }
}

/// Why: no findings is an ordinary outcome and must not panic on the empty
/// `period_order` lookup.
/// What: tags an empty vec, asserts an empty result.
/// Test: this test itself.
#[test]
fn synthesizer_dedup_empty_findings() {
    assert!(assign_trend_tags(Vec::new()).is_empty());
}

// ── Trajectory derivation ─────────────────────────────────────────────────────

/// Why: the trajectory routes action, so each direction and both degenerate
/// cases must be pinned.
/// What: asserts `Improving` for a rising series, `Declining` for a falling one,
/// `Stable` for a flat one, and `Stable` for one and zero points.
/// Test: this test itself.
#[test]
fn synthesizer_trajectory_from_slope() {
    let up = vec![
        ("Q1".to_string(), 2.0),
        ("Q2".to_string(), 3.0),
        ("Q3".to_string(), 4.0),
    ];
    assert_eq!(derive_trajectory(&up), Trajectory::Improving);

    let down = vec![
        ("Q1".to_string(), 4.0),
        ("Q2".to_string(), 3.0),
        ("Q3".to_string(), 2.0),
    ];
    assert_eq!(derive_trajectory(&down), Trajectory::Declining);

    let flat = vec![
        ("Q1".to_string(), 3.0),
        ("Q2".to_string(), 3.1),
        ("Q3".to_string(), 2.9),
    ];
    assert_eq!(derive_trajectory(&flat), Trajectory::Stable);

    assert_eq!(
        derive_trajectory(&[("Q1".to_string(), 3.0)]),
        Trajectory::Stable
    );
    assert_eq!(derive_trajectory(&[]), Trajectory::Stable);
}

// ── Deterministic synthesis ───────────────────────────────────────────────────

/// Why: the quality series is what the report charts and what the trajectory is
/// derived from, so it must come off the period stats in order.
/// What: runs deterministic synthesis over two periods, asserts both labels and
/// scores landed in `quality_trend`.
/// Test: this test itself.
#[test]
fn synthesizer_quality_trend_populated() {
    let mut profile = make_profile();
    let periods = vec![make_period("2026-Q1", 3.0), make_period("2026-Q2", 3.5)];
    apply_deterministic_synthesis(&mut profile, vec![], &periods);

    assert_eq!(profile.quality_trend.len(), 2);
    assert_eq!(profile.quality_trend[0].0, "2026-Q1");
    assert!((profile.quality_trend[0].1 - 3.0).abs() < f64::EPSILON);
    assert_eq!(profile.quality_trend[1].0, "2026-Q2");
}

/// Why: deterministic synthesis is the only stage that flattens per-period
/// findings and tags them, so a profile that skips the narrative pass still
/// needs it to have run end to end.
/// What: passes two periods of findings, asserts they are flattened into
/// `all_findings`, every one is tagged, and the trajectory came from the slope.
/// Test: this test itself.
#[test]
fn synthesizer_deterministic_synthesis_tags_findings() {
    let mut profile = make_profile();
    let periods = vec![make_period("2026-Q1", 2.0), make_period("2026-Q2", 4.0)];
    let per_period = vec![
        vec![make_finding(
            "2026-Q1",
            "missing error propagation in async",
        )],
        vec![make_finding(
            "2026-Q2",
            "missing error propagation in async",
        )],
    ];

    apply_deterministic_synthesis(&mut profile, per_period, &periods);

    assert_eq!(profile.all_findings.len(), 2, "findings must be flattened");
    assert!(
        profile.all_findings.iter().all(|f| f.trend_tag.is_some()),
        "every finding must carry a trend tag"
    );
    assert_eq!(
        profile.improvement_trajectory,
        Trajectory::Improving,
        "2.0 → 4.0 must derive Improving"
    );
}

// ── Narrative response ────────────────────────────────────────────────────────

/// Why: a fenced JSON block inside prose is the legacy answer shape and must
/// still populate every narrative field.
/// What: applies a fenced response, asserts strengths, weaknesses, the
/// overridden trajectory, and the narrative text.
/// Test: this test itself.
#[test]
fn synthesizer_applies_llm_result() {
    let response = r#"Assessment follows.
```json
{
  "strengths": ["Consistent ticket coverage", "Fast cycle times"],
  "recurring_weaknesses": ["Missing error handling"],
  "improvement_trajectory": "improving",
  "narrative": "Alice shows strong improvement over the profile window."
}
```"#;
    let mut profile = make_profile();
    apply_synthesis_json(&mut profile, response);

    assert_eq!(profile.strengths.len(), 2);
    assert_eq!(profile.recurring_weaknesses.len(), 1);
    assert_eq!(profile.improvement_trajectory, Trajectory::Improving);
    assert!(profile.narrative.contains("Alice"));
}

/// Why: with structured output the body is a bare object, so the parser must
/// not require a fence.
/// What: applies a bare object, asserts the fields landed.
/// Test: this test itself.
#[test]
fn synthesizer_applies_direct_json_result() {
    let direct_json = r#"{"strengths":["Good test coverage"],"recurring_weaknesses":["Error handling gaps"],"improvement_trajectory":"improving","narrative":"Bob demonstrates steady improvement."}"#;
    let mut profile = make_profile();
    apply_synthesis_json(&mut profile, direct_json);

    assert_eq!(profile.strengths.len(), 1);
    assert_eq!(profile.strengths[0], "Good test coverage");
    assert_eq!(profile.improvement_trajectory, Trajectory::Improving);
    assert!(profile.narrative.contains("Bob"));
}

/// Why: an unparseable answer must leave a usable profile rather than an empty
/// narrative that reads as "nothing to report".
/// What: applies a prose-only body, asserts the fallback narrative was written.
/// Test: this test itself.
#[test]
fn synthesizer_unparseable_response_falls_back() {
    let mut profile = make_profile();
    apply_synthesis_json(&mut profile, "I could not complete that request.");
    assert!(
        profile
            .narrative
            .contains("Narrative generation unavailable"),
        "unparseable response must apply the fallback: {}",
        profile.narrative
    );
}

/// Why: the deterministic trajectory is the defensible one, so an unrecognised
/// value from the model must not overwrite it.
/// What: sets the trajectory to `Declining`, applies a response naming
/// "sideways", asserts `Declining` survives.
/// Test: this test itself.
#[test]
fn synthesizer_ignores_unknown_trajectory() {
    let mut profile = make_profile();
    profile.improvement_trajectory = Trajectory::Declining;
    apply_synthesis_json(
        &mut profile,
        r#"{"strengths":[],"recurring_weaknesses":[],"improvement_trajectory":"sideways","narrative":"n"}"#,
    );
    assert_eq!(
        profile.improvement_trajectory,
        Trajectory::Declining,
        "an unknown trajectory spelling must leave the derived value alone"
    );
}

/// Why: when no narrative is available the profile must still say who it is
/// about, which way they are trending, and that the prose is a fallback.
/// What: applies the fallback directly, asserts the name, the failure notice,
/// and the recurring count all appear.
/// Test: this test itself.
#[test]
fn synthesizer_fail_safe_narrative() {
    let mut profile = make_profile();
    profile.all_findings = assign_trend_tags(vec![
        make_finding("2026-Q1", "missing error propagation in async"),
        make_finding("2026-Q2", "missing error propagation in async"),
    ]);
    apply_fallback_narrative(&mut profile);

    assert!(
        !profile.narrative.is_empty(),
        "fail-safe must produce a non-empty narrative"
    );
    assert!(
        profile.narrative.contains("Alice"),
        "fail-safe narrative must mention the contributor name"
    );
    assert!(
        profile.narrative.contains("LLM call failed"),
        "fail-safe narrative must indicate the failure: {}",
        profile.narrative
    );
    assert!(
        profile.narrative.contains("2 recurring issue(s)"),
        "fail-safe narrative must count the recurring findings: {}",
        profile.narrative
    );
}

// ── Narrative request ─────────────────────────────────────────────────────────

/// Why: without a `strengths` and `narrative` property in the schema there is
/// nothing for the provider to force, and the parser is back to guessing.
/// What: asserts the schema is an object carrying both properties.
/// Test: this test itself.
#[test]
fn synthesis_output_schema_has_expected_properties() {
    let schema = synthesis_output_schema();
    assert!(schema.is_object(), "schema must be a JSON object");
    let props = &schema["properties"];
    assert!(
        props["strengths"].is_object(),
        "schema must have strengths property"
    );
    assert!(
        props["narrative"].is_object(),
        "schema must have narrative property"
    );
}

/// Why: the system prompt names the fields `apply_synthesis_json` reads, so a
/// drift between the two silently produces fallback narratives.
/// What: asserts the prompt mentions all four output fields.
/// Test: this test itself.
#[test]
fn synthesizer_system_prompt_names_output_fields() {
    let prompt = synthesizer_system_prompt();
    for field in [
        "strengths",
        "recurring_weaknesses",
        "improvement_trajectory",
        "narrative",
    ] {
        assert!(prompt.contains(field), "system prompt must name {field}");
    }
}

/// Why: the narrative call is only grounded if the deterministic results reach
/// it, so the message must carry the score series, the tagged findings, and the
/// computed trajectory.
/// What: builds a message from a synthesised profile and asserts each appears.
/// Test: this test itself.
#[test]
fn synthesizer_user_message_includes_trend_and_findings() {
    let mut profile = make_profile();
    let periods = vec![make_period("2026-Q1", 2.0), make_period("2026-Q2", 4.0)];
    apply_deterministic_synthesis(
        &mut profile,
        vec![
            vec![make_finding(
                "2026-Q1",
                "missing error propagation in async",
            )],
            vec![make_finding(
                "2026-Q2",
                "missing error propagation in async",
            )],
        ],
        &periods,
    );

    let msg = build_synthesizer_user_message(&profile);
    assert!(msg.contains("Alice"), "identity header");
    assert!(msg.contains("2026-Q1"), "quality trend row");
    assert!(msg.contains("error_handling: 2×"), "frequency by kind");
    assert!(msg.contains("[Recurring]"), "trend tag on a sample finding");
    assert!(
        msg.contains("Deterministic trajectory: improving"),
        "the derived trajectory must reach the prompt: {msg}"
    );
}

/// Why: a profile with no findings must still produce a valid prompt rather
/// than an empty findings section the model reads as truncation.
/// What: builds a message from a profile with no findings, asserts the explicit
/// placeholder.
/// Test: this test itself.
#[test]
fn synthesizer_user_message_handles_no_findings() {
    let mut profile = make_profile();
    apply_deterministic_synthesis(&mut profile, vec![], &[make_period("2026-Q1", 3.0)]);
    let msg = build_synthesizer_user_message(&profile);
    assert!(
        msg.contains("no findings extracted"),
        "empty findings must be stated explicitly: {msg}"
    );
}
