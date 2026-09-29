//! Verify every finding about to be posted, after the citation gate (#8904).
//!
//! Why: on trusty-review 0.36.1 the verifier checked only verdict-changing
//! findings and ran in 1 of 10 reviews, so all 7 fabricated findings were
//! posted unverified. #8905 drops findings whose cited line does not hold the
//! quoted code; this module covers the other half — a finding whose claim is
//! false at the right location.
//! What: [`gate_then_verify`] is the one post-grading seam both review paths
//! call: the #8905 citation gate first (a dropped citation saves a verifier
//! call), then `verify::maybe_verify` on the survivors. [`enforce_outcomes`]
//! records each verifier outcome and DROPS three kinds of finding, logging
//! each: one the verifier refuted, one it could not judge (error, timeout,
//! unparseable or truncated answer), and one past the `max_calls` cap. A
//! dropped finding is never posted; the verdict then follows the #8905
//! withhold policy (`citation_gate::verdict::settle_withheld`), so a drop
//! never turns a non-APPROVE verdict into APPROVE, and the body leads with
//! "N findings withheld: …".
//! Test: `verify_posted_tests.rs`.

use std::sync::Arc;

use tracing::warn;

use crate::{
    config::ReviewConfig,
    llm::LlmProvider,
    models::{Finding, ReviewResult, Verdict, VerifyOutcome},
    pipeline::{
        citation_gate::{gate_posted_findings, verdict as withhold},
        diff_analyzer::models::FilteredDiff,
        verify::{VerifierReach, apply_outcome, maybe_verify, rederive_verdict},
    },
};

/// What one verification round did (#8904).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    /// The settled verdict.
    pub verdict: Verdict,
    /// Verifier requests made (retries of one request count once).
    pub calls: usize,
    /// Findings dropped because the verifier refuted them.
    pub refuted: usize,
    /// Findings dropped because the verifier could not judge them.
    pub unjudged: usize,
    /// Findings dropped because they fell past the `max_calls` cap.
    pub over_cap: usize,
    /// `file:line` of every dropped finding that cited a line.
    pub withheld: Vec<String>,
}

impl VerifyReport {
    /// Findings the round dropped, for any reason.
    pub fn dropped(&self) -> usize {
        self.refuted + self.unjudged + self.over_cap
    }

    /// The body line naming what was withheld, or `None` when nothing was.
    pub fn note(&self) -> Option<String> {
        if self.dropped() == 0 {
            return None;
        }
        let parts: Vec<String> = [
            (self.refuted, "refuted by the verifier"),
            (self.unjudged, "the verifier could not judge"),
            (self.over_cap, "past the verifier-call cap"),
        ]
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, why)| format!("{n} {why}"))
        .collect();
        Some(format!(
            "{} findings withheld: not verified ({})",
            self.dropped(),
            parts.join(", ")
        ))
    }
}

/// Record verifier outcomes, drop what may not be posted, settle the verdict.
///
/// Why (#8904): a refuted finding is false; an unjudged or over-cap finding
/// was never checked. Posting either is what put 7 fabrications on PRs, and a
/// label does not keep the fabricated text off the PR, so all three are
/// dropped. A verifier-judged UNVERIFIABLE (#5309) is a judgment, not a
/// failure: it stays, demoted to an advisory that cannot escalate, with its
/// "never verified" caveat.
/// What: applies each outcome with `apply_outcome`; drops `Refuted`, every
/// `VerifierReach::Failed` outcome, and every `over_cap` index, logging each;
/// then settles the verdict — `Unknown` stays `Unknown`; nothing dropped →
/// `rederive_verdict`; anything dropped → `settle_withheld`, which never turns
/// a non-APPROVE verdict into APPROVE but may relax a blocking one. When any
/// finding went unjudged the result is floored at `primary` (the #8653
/// verdict), so a verifier failure never relaxes a review. Refuted and
/// over-cap drops are not floored: with no unjudged finding, either may relax
/// the verdict through `settle_withheld` (owner ruling on #8904, 2026-09-29).
/// Test: `verify_refuted_drops_and_block_is_withheld`,
/// `verify_permanent_transport_failure_is_withheld`,
/// `verify_cap_withholds_findings_past_the_last_call`,
/// `verify_refuting_every_finding_of_a_block_review_is_unknown`,
/// `run_review_withheld_blocker_keeps_its_block_floor`.
pub(crate) fn enforce_outcomes(
    primary: Verdict,
    findings: &mut Vec<Finding>,
    outcomes: Vec<(usize, VerifyOutcome, VerifierReach)>,
    over_cap: &[usize],
    calls: usize,
) -> VerifyReport {
    let mut report = VerifyReport {
        verdict: primary.clone(),
        calls,
        refuted: 0,
        unjudged: 0,
        over_cap: over_cap.len(),
        withheld: Vec::new(),
    };
    let mut drop_reason: Vec<Option<&'static str>> = vec![None; findings.len()];
    for (idx, outcome, reach) in outcomes {
        // #8904: fail closed — only a parseable judgment keeps a finding.
        if matches!(outcome, VerifyOutcome::Refuted) {
            report.refuted += 1;
            drop_reason[idx] = Some("refuted by the verifier");
        } else if reach == VerifierReach::Failed {
            report.unjudged += 1;
            drop_reason[idx] = Some("the verifier could not judge it");
        }
        apply_outcome(&mut findings[idx], outcome);
    }
    for &idx in over_cap {
        drop_reason[idx] = Some("past the verifier-call cap");
    }
    let mut kept = Vec::with_capacity(findings.len());
    for (f, reason) in std::mem::take(findings).into_iter().zip(drop_reason) {
        let Some(reason) = reason else {
            kept.push(f);
            continue;
        };
        warn!(file = %f.file, line = ?f.line, kind = %f.kind, reason, "verifier: withholding finding (#8904)");
        if let Some(line) = f.line {
            report.withheld.push(format!("{}:{line}", f.file));
        }
    }
    *findings = kept;
    report.verdict = if primary == Verdict::Unknown {
        Verdict::Unknown
    } else if report.dropped() == 0 {
        rederive_verdict(primary, findings)
    } else {
        let settled = withhold::settle_withheld(primary.clone(), findings);
        // #8904 (owner ruling 2026-09-29): a verifier failure keeps the #8653
        // floor — the pre-verification verdict — so it never relaxes a review.
        // `Unknown` means withheld, not relaxed, so it is never floored.
        if report.unjudged > 0
            && settled != Verdict::Unknown
            && settled.ordinal() < primary.ordinal()
        {
            primary
        } else {
            settled
        }
    };
    report
}

/// Gate citations, then verify the survivors, on a graded review (#8904).
///
/// Why: both review paths must run the same two gates in the same order, and
/// the citation gate first saves a verifier call on every finding it drops.
/// What: runs `gate_posted_findings` (#8905), then `maybe_verify` on
/// `result.findings` with `result.verdict` as the primary verdict. Records
/// the unjudged and over-cap drops in `result.withheld_unverified_count`. When
/// the round withheld findings, scrubs their citations from the body, prepends
/// the report's note, and on `Unknown` clears the grade and records the note as
/// the error — the same shape the citation gate uses. When no round ran, the
/// findings are posted behind [`not_verified_note`] and the verdict is left
/// alone: with verification enabled but no verifier wired, each finding is
/// marked `Unverifiable` so `unverified_count` raises the #4459 alarm; with
/// verification disabled by config — an operator choice — the findings keep
/// no outcome and are not counted (owner ruling on #8904, 2026-09-29).
/// Test: `run_review_posts_no_refuted_advisory_finding`,
/// `run_review_partial_verifier_outage_reports_the_withheld_count`,
/// `run_review_enabled_without_a_verifier_notes_unverified_findings`,
/// `run_review_disabled_verification_notes_unverified_findings`,
/// `run_review_mapreduce_verifies_findings_from_every_chunk`.
pub(crate) async fn gate_then_verify(
    config: &ReviewConfig,
    verifier: Option<&Arc<dyn LlmProvider>>,
    result: &mut ReviewResult,
    filtered: &FilteredDiff,
    diff: &str,
    per_file: bool,
    author_rationale: Option<&str>,
) {
    gate_posted_findings(result, filtered); // #8905 runs first.
    let primary = result.verdict.clone();
    let Some(report) = maybe_verify(
        config,
        verifier,
        diff,
        per_file,
        primary,
        &mut result.findings,
        author_rationale,
    )
    .await
    else {
        if result.findings.is_empty() {
            return;
        }
        let why = if config.verification.enabled {
            mark_unverifiable(&mut result.findings, NO_VERIFIER);
            NO_VERIFIER
        } else {
            VERIFICATION_DISABLED
        };
        let note = not_verified_note(result.findings.len(), why);
        result.review_body = format!("{note}\n\n{}", result.review_body);
        return;
    };
    result.verdict = report.verdict.clone();
    result.withheld_unverified_count = report.unjudged + report.over_cap;
    let Some(note) = report.note() else {
        return;
    };
    result.review_body = withhold::scrub_body(&result.review_body, &report.withheld);
    result.review_body = format!("{note}\n\n{}", result.review_body);
    if result.verdict == Verdict::Unknown {
        result.grade = None;
        result.error.get_or_insert(note);
    }
}

/// Why verification is enabled yet no verifier is wired: its build failed.
const NO_VERIFIER: &str = "no verifier provider could be built";

/// Why no round ran when `[verification] enabled` is false.
const VERIFICATION_DISABLED: &str = "verification is disabled";

/// Body line for `n` findings posted without a verification round (#8904).
fn not_verified_note(n: usize, why: &str) -> String {
    format!("{n} findings not verified: {why}")
}

/// Mark every finding with no recorded outcome `Unverifiable` (#8904).
///
/// Why: a finding posted because no verifier could be built was never
/// checked, and `count_unverified` counts only a recorded outcome.
/// What: sets `verified` directly, skipping `apply_outcome`'s #5309 demotion,
/// so the verdict and grade stay as graded. A finding a hygiene pass already
/// stamped keeps its own outcome and reason.
/// Test: `run_review_enabled_without_a_verifier_notes_unverified_findings`.
fn mark_unverifiable(findings: &mut [Finding], reason: &str) {
    for f in findings.iter_mut().filter(|f| f.verified.is_none()) {
        f.verified = Some(VerifyOutcome::Unverifiable {
            reason: reason.into(),
        });
    }
}

#[cfg(test)]
#[path = "verify_posted_tests.rs"]
mod tests;
