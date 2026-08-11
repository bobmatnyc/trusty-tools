//! Deterministic cross-period synthesis for the contributor-profile pipeline.
//!
//! Why: per-period findings are independent snapshots. Merging them is what
//! produces the profile's only genuinely longitudinal signal — that the same
//! issue keeps coming back, or stopped. That merge is arithmetic, not
//! judgement, so it runs without a model and stays reproducible.
//! What: [`synthesize_deterministic`] fills the quality trend, clusters
//! findings by Jaccard token-set similarity, assigns a [`TrendTag`] per
//! cluster, derives the [`Trajectory`] from the quality-score slope, and writes
//! a factual narrative. The model-written narrative that replaces that text is
//! #5464 and is deliberately absent here.
//! Test: the `tests` module covers similarity, each trend tag, slope-derived
//! trajectory, and the narrative text.

use tracing::debug;

use super::types::{ContributorProfile, LongitudinalFinding, PeriodBatch, Trajectory, TrendTag};

/// Token-set similarity at or above which two findings are treated as the same
/// underlying issue.
const JACCARD_THRESHOLD: f64 = 0.7;

/// Quality-score slope beyond which a trend counts as directional rather than
/// noise.
const TRAJECTORY_SLOPE_EPSILON: f64 = 0.1;

// ─── Public entry point ──────────────────────────────────────────────────────

/// Run the deterministic half of synthesis over a profile, in place.
///
/// Why: a profile is useful without any model call — the quality trend, the
/// recurring-issue count, and the trajectory are all computable from the data
/// alone, and computing them here means the narrative pass (#5464) can fail
/// without costing the caller the whole profile.
/// What: populates `quality_trend` from the period statistics, flattens and
/// trend-tags every finding, derives `improvement_trajectory` from the slope of
/// the quality trend, and writes [`deterministic_narrative`] into `narrative`.
/// Strengths and weaknesses are left empty — those are judgements, not
/// derivations.
/// Test: `synthesize_deterministic_populates_quality_trend`,
/// `synthesize_deterministic_writes_narrative`.
pub fn synthesize_deterministic(
    profile: &mut ContributorProfile,
    all_period_findings: Vec<Vec<LongitudinalFinding>>,
    periods: &[PeriodBatch],
) {
    profile.quality_trend = periods
        .iter()
        .map(|b| (b.stats.period_label.clone(), b.stats.quality_score))
        .collect();

    let flat: Vec<LongitudinalFinding> = all_period_findings.into_iter().flatten().collect();
    debug!(
        findings = flat.len(),
        periods = periods.len(),
        "synthesize_deterministic"
    );
    profile.all_findings = assign_trend_tags(flat);
    profile.improvement_trajectory = derive_trajectory(&profile.quality_trend);
    profile.narrative = deterministic_narrative(profile);
}

// ─── Jaccard dedup + trend-tag assignment ────────────────────────────────────

/// Assign a [`TrendTag`] to every finding in a flat, all-periods list.
///
/// Why: raw per-period findings carry no trend; without clustering, the same
/// issue reported in four periods reads as four unrelated problems.
/// What: clusters findings whose descriptions have Jaccard similarity ≥ 0.7,
/// then tags each cluster by which periods it spans — present in the latest
/// period and in an earlier one is `Recurring` (or `Worsening` when the latest
/// occurrence's confidence rose by more than 0.1), latest only is `New`,
/// earlier only is `Resolved`.
/// Test: `synthesizer_dedup_assigns_recurring`,
/// `synthesizer_dedup_assigns_new`, `synthesizer_dedup_assigns_resolved`,
/// `synthesizer_dedup_assigns_worsening`.
pub fn assign_trend_tags(findings: Vec<LongitudinalFinding>) -> Vec<LongitudinalFinding> {
    if findings.is_empty() {
        return findings;
    }

    // Period order is insertion order — the caller feeds periods oldest first.
    let mut period_order: Vec<&str> = Vec::new();
    for f in &findings {
        if !period_order.contains(&f.period_label.as_str()) {
            period_order.push(f.period_label.as_str());
        }
    }
    let latest_period = period_order.last().unwrap_or(&"").to_string();

    let clusters = cluster_by_similarity(&findings);

    let mut tagged = findings;
    for cluster in &clusters {
        let in_latest = cluster
            .iter()
            .any(|&idx| tagged[idx].period_label == latest_period);
        let in_earlier = cluster
            .iter()
            .any(|&idx| tagged[idx].period_label != latest_period);

        // Confidence stands in for severity: findings arrive from a review pass
        // that does not carry an explicit severity field.
        let worsening = in_latest && in_earlier && cluster.len() >= 2 && {
            let first_conf = tagged[cluster[0]].finding.confidence;
            let last_conf = tagged[cluster[cluster.len() - 1]].finding.confidence;
            last_conf > first_conf + 0.1
        };

        let tag = match (worsening, in_latest, in_earlier) {
            (true, _, _) => TrendTag::Worsening,
            (_, true, true) => TrendTag::Recurring,
            (_, true, false) => TrendTag::New,
            _ => TrendTag::Resolved,
        };

        for &idx in cluster {
            tagged[idx].trend_tag = Some(tag);
        }
    }

    tagged
}

/// Group finding indices into clusters of mutually similar descriptions.
///
/// Why: keeps [`assign_trend_tags`] readable by separating "which findings are
/// the same issue" from "what does that imply".
/// What: greedy single-pass clustering — each unassigned finding seeds a
/// cluster and absorbs every later finding within [`JACCARD_THRESHOLD`] of it.
/// Test: exercised by every `synthesizer_dedup_*` test.
fn cluster_by_similarity(findings: &[LongitudinalFinding]) -> Vec<Vec<usize>> {
    let mut clusters: Vec<Vec<usize>> = Vec::new();
    let mut assigned = vec![false; findings.len()];

    for i in 0..findings.len() {
        if assigned[i] {
            continue;
        }
        let mut cluster = vec![i];
        assigned[i] = true;
        for j in (i + 1)..findings.len() {
            if assigned[j] {
                continue;
            }
            if jaccard_similarity(
                &findings[i].finding.description,
                &findings[j].finding.description,
            ) >= JACCARD_THRESHOLD
            {
                cluster.push(j);
                assigned[j] = true;
            }
        }
        clusters.push(cluster);
    }

    clusters
}

/// Jaccard similarity between two finding descriptions.
///
/// Why: finding descriptions are short technical phrases whose wording varies
/// between periods; token overlap tracks "same issue" well enough without
/// pulling in an embedding model.
/// What: tokenises both strings into lowercase alphanumeric words and returns
/// `|intersection| / |union|`. Two empty strings are identical (1.0); one empty
/// string shares nothing (0.0).
/// Test: `jaccard_similarity_basic`, `jaccard_similarity_similar_descriptions`.
pub fn jaccard_similarity(a: &str, b: &str) -> f64 {
    let tokens_a = tokenize(a);
    let tokens_b = tokenize(b);
    if tokens_a.is_empty() && tokens_b.is_empty() {
        return 1.0;
    }
    if tokens_a.is_empty() || tokens_b.is_empty() {
        return 0.0;
    }
    let intersection = tokens_a.iter().filter(|t| tokens_b.contains(t)).count();
    let union = tokens_a.len() + tokens_b.len() - intersection;
    intersection as f64 / union as f64
}

/// Tokenise a string into lowercase alphanumeric tokens.
fn tokenize(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

// ─── Trajectory derivation ───────────────────────────────────────────────────

/// Derive a [`Trajectory`] from the quality-score series.
///
/// Why: comparing only the first and last period would let one bad quarter
/// dictate the verdict; a least-squares slope uses every period.
/// What: fits a line over the `(index, score)` pairs and classifies the slope —
/// above +0.1 is `Improving`, below −0.1 is `Declining`, anything between is
/// `Stable`. Fewer than two points is `Stable` by definition.
/// Test: `synthesizer_trajectory_from_slope`.
pub fn derive_trajectory(quality_trend: &[(String, f64)]) -> Trajectory {
    if quality_trend.len() < 2 {
        return Trajectory::Stable;
    }
    let n = quality_trend.len() as f64;
    let sum_x: f64 = (0..quality_trend.len()).map(|i| i as f64).sum();
    let sum_y: f64 = quality_trend.iter().map(|(_, s)| s).sum();
    let sum_xy: f64 = quality_trend
        .iter()
        .enumerate()
        .map(|(i, (_, s))| i as f64 * s)
        .sum();
    let sum_xx: f64 = (0..quality_trend.len())
        .map(|i| (i as f64) * (i as f64))
        .sum();
    let denom = n * sum_xx - sum_x * sum_x;
    if denom.abs() < f64::EPSILON {
        return Trajectory::Stable;
    }
    let slope = (n * sum_xy - sum_x * sum_y) / denom;
    if slope > TRAJECTORY_SLOPE_EPSILON {
        Trajectory::Improving
    } else if slope < -TRAJECTORY_SLOPE_EPSILON {
        Trajectory::Declining
    } else {
        Trajectory::Stable
    }
}

// ─── Narrative ───────────────────────────────────────────────────────────────

/// Build a factual narrative from the deterministic parts of a profile.
///
/// Why: a profile whose narrative section is empty reads as broken. This text
/// states only what was computed, so it is also the correct fallback when the
/// narrative pass (#5464) fails.
/// What: names the contributor and window, the trajectory, and how many
/// findings carry the `Recurring` tag. That count is occurrences, not distinct
/// issues — one issue seen in three periods contributes three — so the wording
/// says "finding(s) tagged recurring" rather than "recurring issues".
/// Test: `synthesize_deterministic_writes_narrative`.
pub fn deterministic_narrative(profile: &ContributorProfile) -> String {
    let traj_str = match profile.improvement_trajectory {
        Trajectory::Improving => "improving",
        Trajectory::Stable => "stable",
        Trajectory::Declining => "declining",
    };
    let n_recurring = profile
        .all_findings
        .iter()
        .filter(|f| f.trend_tag == Some(TrendTag::Recurring))
        .count();
    format!(
        "Longitudinal profile for {} ({} to {}). \
         Quality trajectory: {}. \
         {} finding(s) tagged recurring across {} period(s).",
        profile.canonical_name,
        profile.profiled_since,
        profile.profiled_until,
        traj_str,
        n_recurring,
        profile.quality_trend.len(),
    )
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::types::{FindingEffort, ProfileFinding};
    use std::collections::HashMap;

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
            finding: ProfileFinding::new(
                "src/lib.rs",
                "error_handling",
                description,
                "fix it",
                confidence,
                FindingEffort::Medium,
            ),
            trend_tag: None,
        }
    }

    fn make_profile() -> ContributorProfile {
        ContributorProfile::new("alice@example.com", "Alice", "2026-01-01", "2026-12-31")
    }

    fn make_period(label: &str, score: f64) -> PeriodBatch {
        PeriodBatch::from_stats(crate::report::period_trends::AuthorPeriodSummary {
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

    // ── Jaccard ──────────────────────────────────────────────────────────

    /// Why: clustering is only as good as the similarity measure, so its three
    /// boundary cases have to be pinned.
    /// What: asserts identity scores 1.0, unrelated phrases score below 0.5,
    /// two empty strings score 1.0, and one empty string scores 0.0.
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

    /// Why: the same issue is rarely described in the same words twice, so
    /// near-matches must score high enough to cluster.
    /// What: two rewordings of one issue must score at least 0.6.
    /// Test: this test itself.
    #[test]
    fn jaccard_similarity_similar_descriptions() {
        let a = "missing error propagation in async function";
        let b = "missing error propagation async function handler";
        let sim = jaccard_similarity(a, b);
        assert!(sim >= 0.6, "similar descriptions: sim={sim:.3}");
    }

    // ── Trend tags ───────────────────────────────────────────────────────

    /// Why: an issue present in the latest period and an earlier one is the
    /// recurring case the whole feature exists to surface.
    /// What: tags two identically-described findings in different periods and
    /// asserts both come back `Recurring`.
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
            assert_eq!(f.trend_tag, Some(TrendTag::Recurring));
        }
    }

    /// Why: a first-time issue must not be reported as a pattern.
    /// What: tags a single finding in the only period and asserts `New`.
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

    /// Why: an issue that stopped appearing is a positive signal, and reporting
    /// it as still-recurring would be actively misleading.
    /// What: puts one issue in Q1 only and an unrelated one in Q2, then asserts
    /// the Q1 finding is `Resolved`.
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
        assert_eq!(q1.trend_tag, Some(TrendTag::Resolved));
    }

    /// Why: a recurring issue reported with rising confidence is a different
    /// signal from one holding steady, and only `Worsening` says so.
    /// What: repeats one issue across two periods with confidence rising from
    /// 0.5 to 0.9 and asserts `Worsening` rather than `Recurring`.
    /// Test: this test itself.
    #[test]
    fn synthesizer_dedup_assigns_worsening() {
        let findings = vec![
            make_finding_with_confidence("2026-Q1", "unchecked index into the buffer", 0.5),
            make_finding_with_confidence("2026-Q2", "unchecked index into the buffer", 0.9),
        ];
        let tagged = assign_trend_tags(findings);
        for f in &tagged {
            assert_eq!(f.trend_tag, Some(TrendTag::Worsening));
        }
    }

    /// Why: a contributor with no findings is the good case, and it must not
    /// panic on the empty-period-list path.
    /// What: tags an empty vector and asserts it stays empty.
    /// Test: this test itself.
    #[test]
    fn synthesizer_dedup_empty_findings() {
        assert!(assign_trend_tags(Vec::new()).is_empty());
    }

    // ── Trajectory ───────────────────────────────────────────────────────

    /// Why: the trajectory is the profile's headline, and a noisy-but-flat
    /// series must not read as a trend.
    /// What: asserts ascending → `Improving`, descending → `Declining`, jittery
    /// → `Stable`, and that one or zero points is `Stable`.
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

    // ── Deterministic synthesis ──────────────────────────────────────────

    /// Why: the quality trend is what the renderer charts and what the
    /// trajectory is derived from, so it has to come straight from the period
    /// statistics in order.
    /// What: synthesises over two periods and asserts labels, scores, and the
    /// resulting `Improving` trajectory.
    /// Test: this test itself.
    #[test]
    fn synthesize_deterministic_populates_quality_trend() {
        let mut profile = make_profile();
        let periods = vec![make_period("2026-Q1", 3.0), make_period("2026-Q2", 3.8)];
        synthesize_deterministic(&mut profile, vec![], &periods);

        assert_eq!(profile.quality_trend.len(), 2);
        assert_eq!(profile.quality_trend[0].0, "2026-Q1");
        assert!((profile.quality_trend[0].1 - 3.0).abs() < f64::EPSILON);
        assert_eq!(profile.improvement_trajectory, Trajectory::Improving);
        assert!(
            profile.strengths.is_empty(),
            "strengths are a judgement, not a derivation"
        );
    }

    /// Why: the report renders the narrative section only when it is non-empty,
    /// so a deterministic run that left it blank would produce a profile with a
    /// hole where its summary belongs.
    /// What: synthesises with one issue recurring across two periods and
    /// asserts the narrative names the contributor, the trajectory, and the
    /// recurring count — which is occurrences (2), not distinct issues (1).
    /// Test: this test itself.
    #[test]
    fn synthesize_deterministic_writes_narrative() {
        let mut profile = make_profile();
        let periods = vec![make_period("2026-Q1", 3.0), make_period("2026-Q2", 3.8)];
        let findings = vec![
            vec![make_finding("2026-Q1", "missing error propagation")],
            vec![make_finding("2026-Q2", "missing error propagation")],
        ];
        synthesize_deterministic(&mut profile, findings, &periods);

        assert!(profile.narrative.contains("Alice"), "names the contributor");
        assert!(profile.narrative.contains("improving"), "states trajectory");
        assert!(
            profile.narrative.contains("2 finding(s) tagged recurring"),
            "counts recurring findings: {}",
            profile.narrative
        );
        assert_eq!(profile.all_findings.len(), 2);
    }
}
