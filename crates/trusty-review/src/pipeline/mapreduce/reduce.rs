//! Reduce stage — aggregate per-chunk outcomes into one `ReducedReview`
//! (Phase 4, #1643 / #680).
//!
//! Why: the map stage produces N independent per-file outcomes; the reduce stage
//! must collapse them into ONE verdict + finding set with the SAME shape the
//! unified path produces, so the downstream grade / verify / post code is
//! unchanged.  The verdict MUST be derived deterministically from findings (never
//! let a summariser silently downgrade), consistent with the existing
//! `grade::derive_verdict` precedence and clamp rules.
//!
//! What: `reduce` (1) unions all per-chunk findings, (2) dedups same-file
//! findings by Jaccard similarity ≥ `FINDING_SIMILARITY_THRESHOLD` (reusing
//! `profile::synthesizer::jaccard_similarity`), (3) prioritises by (Effort,
//! confidence) and caps at `config.max_findings`, then (4) derives the overall
//! verdict via `grade::derive_verdict` seeded by the STRICTER-OF all per-chunk
//! verdicts (so a chunk REQUEST_CHANGES/BLOCK propagates to the whole review).
//! Per-chunk UNKNOWN does NOT poison — only an all-UNKNOWN, no-findings review
//! collapses to UNKNOWN.
//!
//! Test: `mapreduce/reduce_tests.rs`.

use tracing::{debug, info};

use crate::{
    config::{constants::FINDING_SIMILARITY_THRESHOLD, mapreduce::MapReduceConfig},
    models::{Effort, Finding, Verdict},
    pipeline::grade::derive_verdict,
    profile::synthesizer::jaccard_similarity,
};

use super::outcome::{MapOutcome, MapReduceStats, ReducedReview, TokenUsage};

/// Reduce a vec of per-chunk `MapOutcome`s into a single `ReducedReview`.
///
/// Why: this is the deterministic aggregation that makes a per-file map-reduce
/// review indistinguishable (in shape) from a unified review.  Determinism is
/// essential — the verdict is grounded in the union of findings, not in any
/// LLM-authored summary, so a synthesiser pass can never silently soften a real
/// blocking finding.
/// What: builds `MapReduceStats`, unions + dedups + caps the findings, derives
/// the verdict from the stricter-of all per-chunk verdicts + the severity floor,
/// sums each reviewed chunk's `TokenUsage` into the aggregate the shallow-review
/// heuristic reads (#1885), and returns the merged `ReducedReview`.
/// Test: `reduce_unions_findings`, `reduce_chunk_request_changes_propagates`,
/// `reduce_dedups_identical_findings`, `reduce_all_unknown_collapses`,
/// `reduce_caps_findings`, `reduce_stats_partial_flag`, `reduce_sums_output_tokens`.
pub fn reduce(outcomes: Vec<MapOutcome>, config: &MapReduceConfig) -> ReducedReview {
    let mut stats = MapReduceStats {
        units_total: outcomes.len(),
        ..Default::default()
    };

    // Collect the per-chunk verdicts (Reviewed only) and the finding union.
    let mut chunk_verdicts: Vec<Verdict> = Vec::new();
    let mut all_findings: Vec<Finding> = Vec::new();
    // Running total of token/cost telemetry across every reviewed chunk — this is
    // the aggregate the shallow-review heuristic reads on the map-reduce path
    // (#1885). Skipped/failed chunks made no LLM call, so they contribute nothing.
    let mut tokens = TokenUsage::default();

    for outcome in outcomes {
        match outcome {
            MapOutcome::Reviewed {
                verdict,
                findings,
                tokens: chunk_tokens,
                ..
            } => {
                stats.files_reviewed += 1;
                chunk_verdicts.push(verdict);
                all_findings.extend(findings);
                tokens = tokens.merged(chunk_tokens);
            }
            MapOutcome::Skipped { .. } => {
                stats.files_skipped += 1;
            }
            MapOutcome::Failed {
                file,
                error,
                hunk_oversized,
            } => {
                stats.files_failed += 1;
                if hunk_oversized {
                    stats.hunks_oversized += 1;
                }
                debug!(file = %file, %error, hunk_oversized, "reduce: dropped failed chunk");
            }
        }
    }

    // Dedup the same-file finding union, then prioritise + cap.
    let deduped = dedup_findings(all_findings);
    let findings = prioritise_and_cap(deduped, config.max_findings);
    stats.findings_surfaced = findings.len();

    // Derive the aggregate verdict deterministically.
    let verdict = aggregate_verdict(&chunk_verdicts, &findings);

    info!(
        units = stats.units_total,
        reviewed = stats.files_reviewed,
        skipped = stats.files_skipped,
        failed = stats.files_failed,
        hunks_oversized = stats.hunks_oversized,
        findings = stats.findings_surfaced,
        verdict = %verdict,
        partial = stats.is_partial(),
        "reduce stage complete"
    );

    ReducedReview {
        verdict,
        findings,
        stats,
        // grade, grade_pre_floor, and summary are populated by the optional
        // synthesis pass (#1663 / #1665); the mechanical reduce stage leaves
        // them absent.
        grade: None,
        grade_pre_floor: None,
        summary: String::new(),
        // Map-stage token total; the synthesis pass adds its own call on top.
        tokens,
    }
}

/// Derive the overall verdict from the per-chunk verdicts + the deduped findings.
///
/// Why: the verdict must reflect the WORST per-chunk judgement (a single chunk
/// REQUEST_CHANGES/BLOCK must propagate to the whole review) AND the severity
/// floor implied by the aggregate finding set — exactly the precedence the
/// unified path applies via `derive_verdict`.  Doing this deterministically (not
/// via an LLM summary) is the issue's hard requirement.
/// What:
///   - No reviewed chunks at all → `Unknown` (nothing was assessed).
///   - All reviewed chunks `Unknown` → `Unknown` (the diff was unassessable).
///   - Otherwise seed `derive_verdict` with the STRICTER-OF the non-UNKNOWN
///     per-chunk verdicts (UNKNOWN is dropped so one unassessable chunk does not
///     poison an otherwise-clean review), letting the existing severity floor /
///     low-confidence override apply over the deduped findings.
///
/// Test: `reduce_chunk_request_changes_propagates`, `reduce_all_unknown_collapses`,
/// `reduce_unknown_chunk_does_not_poison`.
fn aggregate_verdict(chunk_verdicts: &[Verdict], findings: &[Finding]) -> Verdict {
    if chunk_verdicts.is_empty() {
        // Nothing was reviewed (all skipped/failed) — could not assess.
        return Verdict::Unknown;
    }

    // Drop UNKNOWN chunks: an unassessable chunk must not poison the review, but
    // if EVERY reviewed chunk was UNKNOWN the whole review is unassessable.
    let non_unknown: Vec<&Verdict> = chunk_verdicts
        .iter()
        .filter(|v| **v != Verdict::Unknown)
        .collect();
    if non_unknown.is_empty() {
        return Verdict::Unknown;
    }

    // Stricter-of all non-UNKNOWN chunk verdicts (by ordinal severity).  This is
    // the precedence BLOCK > REQUEST_CHANGES > APPROVE* > APPROVE from #680.
    //
    // IMPORTANT: the UNKNOWN filter above MUST precede this `max_by_key`, because
    // `Verdict::Unknown.ordinal() == 4` is the HIGHEST ordinal — an unfiltered
    // UNKNOWN would be selected as "worst" and poison the seed.  The filter is
    // therefore load-bearing, not cosmetic.
    let worst = non_unknown
        .iter()
        .max_by_key(|v| v.ordinal())
        .map(|v| (*v).clone())
        .unwrap_or(Verdict::Approve);

    // Apply the existing deterministic severity floor / low-confidence override
    // over the aggregate finding set, seeded by the worst chunk verdict.  This
    // reuses the SAME logic as the unified path so the verdict policy never drifts.
    derive_verdict(worst, findings)
}

/// Dedup findings, treating two same-file findings as duplicates when their
/// descriptions are Jaccard-similar above `FINDING_SIMILARITY_THRESHOLD`.
///
/// Why: a single logical issue can surface in more than one chunk of the same
/// file (e.g. a duplicated pattern); surfacing it twice spams the author.
/// Reusing the profile synthesiser's Jaccard metric keeps the similarity rule
/// consistent across the crate.
/// What: keeps the FIRST occurrence; a later finding is dropped only when it
/// shares the same `file` AND the same `kind` AND its `description`
/// Jaccard-similarity to an already kept finding is ≥
/// `FINDING_SIMILARITY_THRESHOLD`.  Requiring matching `kind` prevents merging
/// two genuinely distinct issues (e.g. a `"security"` and a `"logic-error"`
/// finding) that happen to share similar prose.  Findings on different files are
/// never merged (a cross-file coincidence is not a duplicate).
/// Test: `reduce_dedups_identical_findings`, `reduce_keeps_distinct_findings`,
/// `reduce_keeps_same_text_different_kind`.
fn dedup_findings(findings: Vec<Finding>) -> Vec<Finding> {
    let mut kept: Vec<Finding> = Vec::with_capacity(findings.len());
    for f in findings {
        let is_dup = kept.iter().any(|k| {
            k.file == f.file
                && k.kind == f.kind
                && jaccard_similarity(&k.description, &f.description)
                    >= f64::from(FINDING_SIMILARITY_THRESHOLD)
        });
        if !is_dup {
            kept.push(f);
        } else {
            debug!(file = %f.file, kind = %f.kind, "reduce: dropped duplicate finding");
        }
    }
    kept
}

/// Prioritise findings by (Effort desc, confidence desc) and cap at `max`.
///
/// Why: the surfaced finding set must remain actionable — the most severe,
/// highest-confidence findings must survive the cap (never bury a BLOCK-driving
/// finding behind low-effort nits).  This mirrors the design's
/// "prioritize by (Effort, confidence), cap surfaced findings" rule (§2.3).
/// What: stable-sorts by effort rank (High > Medium > Low) then confidence
/// descending, then truncates to `max`.  A `max` of 0 is treated as "no cap"
/// (defensive — config default is 50, never 0).
/// Test: `reduce_caps_findings`, `reduce_prioritises_high_effort`.
fn prioritise_and_cap(mut findings: Vec<Finding>, max: usize) -> Vec<Finding> {
    findings.sort_by(|a, b| {
        effort_rank(&b.effort).cmp(&effort_rank(&a.effort)).then(
            b.confidence
                .partial_cmp(&a.confidence)
                .unwrap_or(std::cmp::Ordering::Equal),
        )
    });
    if max > 0 && findings.len() > max {
        findings.truncate(max);
    }
    findings
}

/// Numeric rank for an `Effort` (higher = more severe).
///
/// Why: `Effort` has no `Ord`; ranking keeps the sort key explicit and local.
/// What: High=2, Medium=1, Low=0.
/// Test: covered transitively by `reduce_prioritises_high_effort`.
fn effort_rank(effort: &Effort) -> u8 {
    match effort {
        Effort::High => 2,
        Effort::Medium => 1,
        Effort::Low => 0,
    }
}

#[cfg(test)]
#[path = "reduce_tests.rs"]
mod tests;
