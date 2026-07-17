//! Unit tests for the reduce stage (`mapreduce::reduce`).
//!
//! Why: the reduce stage is where per-chunk outcomes become one deterministic
//! verdict + finding set; these tests pin the aggregation rules (union, dedup,
//! verdict precedence, all-UNKNOWN collapse, finding cap, partial-coverage
//! stats) so a regression cannot silently soften a verdict or drop a finding.
//! What: drives `reduce` with hand-built `MapOutcome` vectors.
//! Test: this is the test module.

use super::reduce;
use crate::config::mapreduce::MapReduceConfig;
use crate::models::{Effort, Finding, Verdict};
use crate::pipeline::mapreduce::outcome::{MapOutcome, TokenUsage};

fn cfg() -> MapReduceConfig {
    MapReduceConfig::default()
}

/// A finding that is escalation-eligible by default (`code_provable = true`).
///
/// Why: (#PR84 adversarial-review follow-up) `derive_verdict`'s BLOCK floor now
/// requires a High-effort finding to be escalation-eligible (cited or
/// diff-provable) to drive BLOCK.  These reduce-stage tests model GENUINE,
/// diff-provable bugs (e.g. an auth-bypass description), so the shared helper
/// marks them `code_provable` to preserve each test's original intent.
fn finding(file: &str, kind: &str, desc: &str, conf: f32, effort: Effort) -> Finding {
    let mut f = Finding::new(file, kind, desc, "", conf, effort);
    f.code_provable = true;
    f
}

fn reviewed(file: &str, verdict: Verdict, findings: Vec<Finding>) -> MapOutcome {
    reviewed_with_tokens(file, verdict, findings, TokenUsage::default())
}

fn reviewed_with_tokens(
    file: &str,
    verdict: Verdict,
    findings: Vec<Finding>,
    tokens: TokenUsage,
) -> MapOutcome {
    MapOutcome::Reviewed {
        file: file.to_string(),
        verdict,
        findings,
        tokens,
    }
}

/// Findings from distinct files/chunks must all survive into the union.
#[test]
fn reduce_unions_findings() {
    let outcomes = vec![
        reviewed(
            "src/a.rs",
            Verdict::Approve,
            vec![finding(
                "src/a.rs",
                "f1",
                "issue one in a",
                0.9,
                Effort::Low,
            )],
        ),
        reviewed(
            "src/b.rs",
            Verdict::Approve,
            vec![finding(
                "src/b.rs",
                "f2",
                "totally different issue in b",
                0.9,
                Effort::Low,
            )],
        ),
    ];
    let reduced = reduce(outcomes, &cfg());
    assert_eq!(
        reduced.findings.len(),
        2,
        "both findings must survive union"
    );
    assert_eq!(reduced.stats.files_reviewed, 2);
}

/// A single chunk's REQUEST_CHANGES must propagate to the overall verdict — the
/// core "no chunk verdict is lost" guarantee.
#[test]
fn reduce_chunk_request_changes_propagates() {
    let outcomes = vec![
        reviewed("src/a.rs", Verdict::Approve, vec![]),
        // A confident Medium finding floors to REQUEST_CHANGES; the model verdict
        // on this chunk is REQUEST_CHANGES so it must propagate up.
        reviewed(
            "src/b.rs",
            Verdict::RequestChanges,
            vec![finding(
                "src/b.rs",
                "bug",
                "off-by-one in loop bound",
                0.95,
                Effort::Medium,
            )],
        ),
    ];
    let reduced = reduce(outcomes, &cfg());
    assert_ne!(
        reduced.verdict,
        Verdict::Approve,
        "a chunk REQUEST_CHANGES must not be lost — overall verdict is non-APPROVE"
    );
    assert_eq!(reduced.verdict, Verdict::RequestChanges);
}

/// A High-effort (critical) finding in one chunk must floor the whole review to
/// BLOCK regardless of the other chunks approving.
#[test]
fn reduce_high_effort_chunk_blocks() {
    let outcomes = vec![
        reviewed("src/a.rs", Verdict::Approve, vec![]),
        reviewed(
            "src/b.rs",
            Verdict::Block,
            vec![finding(
                "src/b.rs",
                "auth-bypass",
                "skips the auth check entirely",
                0.95,
                Effort::High,
            )],
        ),
    ];
    let reduced = reduce(outcomes, &cfg());
    assert_eq!(
        reduced.verdict,
        Verdict::Block,
        "a critical High-effort chunk floors the whole review to BLOCK"
    );
}

/// Trivially-identical same-file findings must be deduped to one.
#[test]
fn reduce_dedups_identical_findings() {
    let dup = "null pointer dereference on empty input";
    let outcomes = vec![
        reviewed(
            "src/a.rs",
            Verdict::ApproveWithReservations,
            vec![finding("src/a.rs", "npe", dup, 0.8, Effort::Medium)],
        ),
        reviewed(
            "src/a.rs",
            Verdict::ApproveWithReservations,
            vec![finding("src/a.rs", "npe", dup, 0.8, Effort::Medium)],
        ),
    ];
    let reduced = reduce(outcomes, &cfg());
    assert_eq!(
        reduced.findings.len(),
        1,
        "two identical same-file findings must dedup to one"
    );
}

/// Distinct findings on the SAME file must NOT be merged.
#[test]
fn reduce_keeps_distinct_findings() {
    let outcomes = vec![reviewed(
        "src/a.rs",
        Verdict::ApproveWithReservations,
        vec![
            finding(
                "src/a.rs",
                "x",
                "unchecked array index access",
                0.8,
                Effort::Medium,
            ),
            finding(
                "src/a.rs",
                "y",
                "missing await on async database call",
                0.8,
                Effort::Medium,
            ),
        ],
    )];
    let reduced = reduce(outcomes, &cfg());
    assert_eq!(
        reduced.findings.len(),
        2,
        "distinct same-file findings must both survive"
    );
}

/// Two same-file findings with identical description text but DIFFERENT `kind`
/// must NOT be deduped — they are genuinely distinct issues.
#[test]
fn reduce_keeps_same_text_different_kind() {
    let text = "value is used before it is validated";
    let outcomes = vec![reviewed(
        "src/a.rs",
        Verdict::ApproveWithReservations,
        vec![
            finding("src/a.rs", "security", text, 0.8, Effort::Medium),
            finding("src/a.rs", "logic-error", text, 0.8, Effort::Medium),
        ],
    )];
    let reduced = reduce(outcomes, &cfg());
    assert_eq!(
        reduced.findings.len(),
        2,
        "same text but different kind must NOT be deduped"
    );
}

/// When EVERY reviewed chunk is UNKNOWN the whole review collapses to UNKNOWN.
#[test]
fn reduce_all_unknown_collapses() {
    let outcomes = vec![
        reviewed("src/a.rs", Verdict::Unknown, vec![]),
        reviewed("src/b.rs", Verdict::Unknown, vec![]),
    ];
    let reduced = reduce(outcomes, &cfg());
    assert_eq!(reduced.verdict, Verdict::Unknown);
}

/// One UNKNOWN chunk must NOT poison an otherwise-clean review.
#[test]
fn reduce_unknown_chunk_does_not_poison() {
    let outcomes = vec![
        reviewed("src/a.rs", Verdict::Approve, vec![]),
        reviewed("src/b.rs", Verdict::Unknown, vec![]),
    ];
    let reduced = reduce(outcomes, &cfg());
    assert_eq!(
        reduced.verdict,
        Verdict::Approve,
        "a single UNKNOWN chunk must not poison the review (#680 precedence)"
    );
}

/// No reviewed chunk at all → UNKNOWN (nothing was assessed).
#[test]
fn reduce_no_reviewed_chunk_is_unknown() {
    let outcomes = vec![
        MapOutcome::Skipped {
            file: "src/a.rs".to_string(),
            note: "deleted file".to_string(),
        },
        MapOutcome::Failed {
            file: "src/b.rs".to_string(),
            error: "LLM error".to_string(),
            hunk_oversized: false,
        },
    ];
    let reduced = reduce(outcomes, &cfg());
    assert_eq!(reduced.verdict, Verdict::Unknown);
    assert_eq!(reduced.stats.files_reviewed, 0);
    assert_eq!(reduced.stats.files_skipped, 1);
    assert_eq!(reduced.stats.files_failed, 1);
}

/// The findings cap must keep the highest-severity findings and drop the tail.
#[test]
fn reduce_caps_findings() {
    let mut config = cfg();
    config.max_findings = 2;
    let outcomes = vec![reviewed(
        "src/a.rs",
        Verdict::Block,
        vec![
            finding("src/a.rs", "low1", "minor style nit one", 0.9, Effort::Low),
            finding(
                "src/a.rs",
                "high1",
                "critical security hole alpha",
                0.95,
                Effort::High,
            ),
            finding("src/a.rs", "low2", "minor style nit two", 0.9, Effort::Low),
        ],
    )];
    let reduced = reduce(outcomes, &config);
    assert_eq!(reduced.findings.len(), 2, "findings capped at max_findings");
    // The High-effort finding must survive the cap (prioritised first).
    assert!(
        reduced.findings.iter().any(|f| f.effort == Effort::High),
        "the High-effort finding must survive the cap"
    );
    assert_eq!(reduced.stats.findings_surfaced, 2);
}

/// Guard the load-bearing ordinal invariant: `Verdict::Unknown` MUST have the
/// highest ordinal of all variants so the UNKNOWN filter in `aggregate_verdict`
/// is sufficient to prevent poisoning.
///
/// Why: `aggregate_verdict` filters UNKNOWN before `max_by_key(ordinal())`.
/// If a future enum reorder gives Unknown a lower ordinal than Block, the filter
/// would still be correct — but this test catches any regression where Unknown's
/// ordinal moves below an existing non-Unknown variant and the filter assumption
/// is broken in both directions.
/// What: asserts Unknown.ordinal() > every other variant's ordinal.
/// Test: this test IS the guard.
#[test]
fn verdict_unknown_ordinal_is_highest_invariant() {
    let non_unknown_verdicts = [
        Verdict::Approve,
        Verdict::ApproveWithReservations,
        Verdict::RequestChanges,
        Verdict::Block,
    ];
    for v in &non_unknown_verdicts {
        assert!(
            Verdict::Unknown.ordinal() > v.ordinal(),
            "Verdict::Unknown.ordinal() ({}) must be > {v}.ordinal() ({}) \
             (load-bearing for aggregate_verdict UNKNOWN filter in reduce.rs)",
            Verdict::Unknown.ordinal(),
            v.ordinal()
        );
    }
}

/// Partial-coverage stats must flag a failed/oversized chunk.
#[test]
fn reduce_stats_partial_flag() {
    let outcomes = vec![
        reviewed("src/a.rs", Verdict::Approve, vec![]),
        MapOutcome::Failed {
            file: "src/big.rs".to_string(),
            error: "single hunk over cap".to_string(),
            hunk_oversized: true,
        },
    ];
    let reduced = reduce(outcomes, &cfg());
    assert!(
        reduced.stats.is_partial(),
        "partial coverage must be flagged"
    );
    assert_eq!(reduced.stats.hunks_oversized, 1);
    assert_eq!(reduced.stats.files_failed, 1);
    // The clean chunk still yields a usable APPROVE.
    assert_eq!(reduced.verdict, Verdict::Approve);
}

/// The reduce stage must SUM token/cost telemetry across every reviewed chunk so
/// the runner can feed the aggregate to the shallow-review heuristic (#1885).
/// Skipped/failed chunks made no LLM call and must contribute nothing.
#[test]
fn reduce_sums_output_tokens() {
    let outcomes = vec![
        reviewed_with_tokens(
            "src/a.rs",
            Verdict::Approve,
            vec![],
            TokenUsage {
                input_tokens: 100,
                output_tokens: 7,
                cost_usd: 0.001,
            },
        ),
        reviewed_with_tokens(
            "src/b.rs",
            Verdict::Approve,
            vec![],
            TokenUsage {
                input_tokens: 200,
                output_tokens: 11,
                cost_usd: 0.002,
            },
        ),
        // A failed chunk (no LLM usage recorded) must not perturb the totals.
        MapOutcome::Failed {
            file: "src/c.rs".to_string(),
            error: "LLM error".to_string(),
            hunk_oversized: false,
        },
    ];
    let reduced = reduce(outcomes, &cfg());
    assert_eq!(reduced.tokens.input_tokens, 300, "input tokens must sum");
    assert_eq!(reduced.tokens.output_tokens, 18, "output tokens must sum");
    assert!(
        (reduced.tokens.cost_usd - 0.003).abs() < 1e-9,
        "cost must sum, got {}",
        reduced.tokens.cost_usd
    );
}
