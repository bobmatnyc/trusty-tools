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
//! records each verifier outcome and keeps only CONFIRMED findings (owner
//! ruling on #8905, 2026-09-30). It drops one the verifier refuted, one it
//! could not judge (error, timeout, unparseable or truncated answer), one past
//! the `max_calls` cap, and one judged or pre-stamped UNVERIFIABLE, logging
//! each and recording each in `ReviewResult::withheld_findings`. A dropped
//! finding is never posted; the verdict then follows the #8905 withhold policy
//! (`citation_gate::verdict::settle_withheld`), so a drop never turns a
//! non-APPROVE verdict into APPROVE, and the body leads with one
//! "N findings withheld:" headline counting every withheld finding (#9310).
//! Test: `verify_posted_tests.rs`.

use std::sync::Arc;

use tracing::warn;

use crate::{
    config::ReviewConfig,
    llm::LlmProvider,
    models::{Finding, ReviewResult, Verdict, VerifyOutcome, WithheldFinding},
    pipeline::{
        citation_gate::{
            DocCorpus, LineIndex, gate_posted_findings_with_index, verdict as withhold,
        },
        diff_analyzer::models::FilteredDiff,
        verdict_status::{Judged, apply_grade_floor, apply_withheld_outcome},
        verify::{VerifierReach, apply_outcome, maybe_verify, rederive_verdict},
        withheld_contract as contract,
    },
};

/// `WithheldFinding::reason` for a finding the verifier refuted.
pub const REFUTED_REASON: &str = "refuted by the verifier";
/// `WithheldFinding::reason` for a finding the verifier could not judge.
pub const UNJUDGED_REASON: &str = "the verifier could not judge it";
/// `WithheldFinding::reason` for a finding past the verifier-call cap.
pub const OVER_CAP_REASON: &str = "past the verifier-call cap";
/// `WithheldFinding::reason` for a finding judged or pre-stamped
/// `Unverifiable` (owner ruling on #8905, 2026-09-30).
pub const UNVERIFIABLE_REASON: &str = "unverifiable";
/// `WithheldFinding::reason` for any other finding the verifier did not
/// confirm (for example a pre-recorded `Skipped`).
pub const UNCONFIRMED_REASON: &str = "not confirmed by the verifier";

/// What one verification round did (#8904).
///
/// No `PartialEq`/`Eq`: `withheld_findings` holds `Finding`, which implements
/// neither (#4044).
#[derive(Debug, Clone)]
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
    /// Findings dropped because they were judged or pre-stamped
    /// `Unverifiable` (#4044; owner ruling on #8905, 2026-09-30).
    pub unverifiable: usize,
    /// Findings dropped because they carried any other non-CONFIRMED outcome.
    pub unconfirmed: usize,
    /// `file:line` of every dropped finding that cited a line.
    pub withheld: Vec<String>,
    /// Every dropped finding with its reason (#4044).
    pub withheld_findings: Vec<WithheldFinding>,
}

impl VerifyReport {
    /// Findings the round dropped, for any reason.
    pub fn dropped(&self) -> usize {
        self.refuted + self.unjudged + self.over_cap + self.unverifiable + self.unconfirmed
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
            (self.unverifiable, "unverifiable"),
            (self.unconfirmed, "not confirmed"),
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
/// dropped. #4044 (owner ruling on #8905, 2026-09-30): only a CONFIRMED
/// finding is posted, so an UNVERIFIABLE one — judged by the verifier (#5309)
/// or pre-stamped by a hygiene pass (#4081) — is withheld too.
/// What: applies each outcome with `apply_outcome`; drops `Refuted`, every
/// `VerifierReach::Failed` outcome, every `over_cap` index, and every other
/// finding whose outcome is not `Confirmed` (reason `"unverifiable"` for
/// `Unverifiable`), logging each and recording each in
/// `report.withheld_findings`; then settles the verdict — `Unknown` stays
/// `Unknown`; nothing dropped, or an APPROVE/APPROVE* review that lost only
/// advisory unverifiable findings (the #8949 rule) and kept at least one
/// → `rederive_verdict`;
/// anything else dropped → `settle_withheld`, which keeps an approving verdict
/// even with no survivor (AQ-7t), never turns a non-APPROVE verdict into
/// APPROVE, and may relax a blocking one. When any finding went
/// unjudged the result is floored at `primary` (the #8653 verdict), so a
/// verifier failure never relaxes a review. Refuted and over-cap drops are not
/// floored: with no unjudged finding, either may relax the verdict through
/// `settle_withheld` (owner ruling on #8904, 2026-09-29).
/// Test: `verify_refuted_drops_and_block_is_withheld`,
/// `verify_permanent_transport_failure_is_withheld`,
/// `verify_cap_withholds_findings_past_the_last_call`,
/// `verify_refuting_every_finding_of_a_block_review_is_unknown`,
/// `run_review_withheld_blocker_keeps_its_block_floor`,
/// `run_review_records_a_refuted_finding_as_withheld`,
/// `verify_unverifiable_finding_is_withheld_not_posted`,
/// `verify_unverifiable_advisory_keeps_an_approving_verdict`,
/// `verify_unverifiable_advisory_with_no_survivor_keeps_approve_star`,
/// `verify_unverifiable_finding_of_a_blocking_review_with_no_survivor_is_unknown`.
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
        unverifiable: 0,
        unconfirmed: 0,
        withheld: Vec::new(),
        withheld_findings: Vec::new(),
    };
    let mut drop_reason: Vec<Option<&'static str>> = vec![None; findings.len()];
    for (idx, outcome, reach) in outcomes {
        // #8904: fail closed — only a parseable judgment keeps a finding.
        if matches!(outcome, VerifyOutcome::Refuted) {
            report.refuted += 1;
            drop_reason[idx] = Some(REFUTED_REASON);
        } else if reach == VerifierReach::Failed {
            report.unjudged += 1;
            drop_reason[idx] = Some(UNJUDGED_REASON);
        }
        apply_outcome(&mut findings[idx], outcome);
    }
    for &idx in over_cap {
        drop_reason[idx] = Some(OVER_CAP_REASON);
    }
    // #4044 (owner ruling on #8905, 2026-09-30): post only CONFIRMED findings.
    for (f, reason) in findings.iter().zip(drop_reason.iter_mut()) {
        if reason.is_some() {
            continue;
        }
        match &f.verified {
            Some(VerifyOutcome::Confirmed) => {}
            Some(VerifyOutcome::Unverifiable { .. }) => {
                report.unverifiable += 1;
                *reason = Some(UNVERIFIABLE_REASON);
            }
            _ => {
                report.unconfirmed += 1;
                *reason = Some(UNCONFIRMED_REASON);
            }
        }
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
        report.withheld_findings.push(WithheldFinding {
            finding: f,
            reason: reason.to_string(),
            missing_fragment: None,
        });
    }
    *findings = kept;
    // #8949 rule, applied to the advisory findings #4044 now withholds: an
    // approving review that lost only those keeps its verdict.
    let advisory_only = report.dropped() == report.unverifiable
        && report
            .withheld_findings
            .iter()
            .all(|w| withhold::is_advisory(&w.finding));
    let approving = matches!(primary, Verdict::Approve | Verdict::ApproveWithReservations);
    // With no survivor there is nothing to re-derive from: `settle_withheld`
    // returns an approving verdict unchanged (AQ-7t) and a blocking one `Unknown`.
    report.verdict = if primary == Verdict::Unknown {
        Verdict::Unknown
    } else if report.dropped() == 0 || (approving && advisory_only && !findings.is_empty()) {
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

/// What the post-grading gates read besides the result (#8904, #9188).
pub(crate) struct GateInputs<'a> {
    /// The filtered diff the reviewer saw; citations resolve against it.
    pub(crate) filtered: &'a FilteredDiff,
    /// The diff text the verifier is shown.
    pub(crate) diff: &'a str,
    /// Show the verifier only each finding's own file sections.
    pub(crate) per_file: bool,
    /// PR description and discussion for the verifier (#1618).
    pub(crate) author_rationale: Option<&'a str>,
    /// #9188 D: the fetched context a `[jira:]`/`[gh:]`/`[confluence:]`
    /// citation must resolve in.
    pub(crate) refs: &'a str,
    /// #9193: doc text read at the head that a `[doc:]` citation must quote.
    pub(crate) docs: &'a DocCorpus,
    /// #9188 C: the model-written prose inside `review_body`; #9310: replaced
    /// by the verified summary, never posted.
    pub(crate) narrative: &'a str,
    /// The model's verdict when `relax_verdict_if_evidence_wiped` relaxed it
    /// before grading; `settle_no_survivors` decides from it (#9188, option A).
    pub(crate) wiped_model_verdict: Option<Verdict>,
    /// #9310: the reviewer's own verdict (`verdict_status::judged_verdict`),
    /// which the withheld mapping reads, and its grade floor (ruling 50).
    pub(crate) judged: Judged,
}

/// Gate citations, then verify the survivors, on a graded review (#8904).
///
/// Why: both review paths must run the same two gates in the same order, and
/// the citation gate first saves a verifier call on every finding it drops.
/// What: runs the #8905 citation gate (context citations resolve in
/// `inputs.refs`), then `maybe_verify` on `result.findings` with
/// `result.verdict` as the primary verdict. Records the unjudged, over-cap and
/// unverifiable drops in `result.withheld_unverified_count`, and every drop
/// with its reason in `result.withheld_findings` (#4044). When the round
/// withheld findings, scrubs their citations from the body, and on `Unknown`
/// clears the grade and records the round's note as the error — the same
/// shape the citation gate uses. When no round ran,
/// [`withhold_unverified`] withholds every finding (Bob's "withhold all"
/// ruling, 2026-09-30); a blocking review is UNKNOWN, an approving one keeps
/// its verdict (AQ-7t). #9188 then re-checks every
/// survivor at the head (L), makes a blocking review with no survivor
/// `Unknown` (A, J; an approving one keeps its verdict, AQ-7t), and replaces
/// the model's prose with the summary built from the survivors (C; #9310
/// owner ruling D2, "Always template").
/// #9310: then `verdict_status::apply_withheld_outcome` maps a withheld
/// review from the reviewer's own verdict (`all_withheld` APPROVE,
/// `suppressed_reject` REQUEST_CHANGES), `verdict_status::apply_grade_floor`
/// holds a D or F review at its grade floor (owner ruling 50), and one
/// headline counting `withheld_findings` leads the body
/// (`prepend_withheld_headline`).
/// Test: `run_review_posts_no_refuted_advisory_finding`,
/// `run_review_partial_verifier_outage_reports_the_withheld_count`,
/// `run_review_enabled_without_a_verifier_withholds_every_finding`,
/// `run_review_disabled_verification_withholds_every_finding`,
/// `run_review_mapreduce_verifies_findings_from_every_chunk`,
/// `run_review_posts_a_confirmed_removal_finding_at_its_deletion_line`,
/// `run_review_writes_the_template_for_a_clean_review_byte_for_byte`,
/// `run_review_all_withheld_request_changes_is_suppressed_reject`,
/// `the_withheld_headline_counts_the_whole_array`,
/// `f_with_one_confirmed_low_confidence_finding_reads_block`.
pub(crate) async fn gate_then_verify(
    config: &ReviewConfig,
    verifier: Option<&Arc<dyn LlmProvider>>,
    result: &mut ReviewResult,
    inputs: &GateInputs<'_>,
) {
    let error_before = result.error.clone(); // #9310: restored if the mapping replaces UNKNOWN
    let narrative = contract::take_narrative(result, inputs.narrative);
    let index = LineIndex::from_filtered(inputs.filtered)
        .with_refs(inputs.refs)
        .with_docs(inputs.docs); // #9193: the gate and the re-check (L) both read it
    gate_posted_findings_with_index(result, &index); // #8905 runs first.
    verify_survivors(config, verifier, result, inputs).await;
    contract::withhold_unresolved(result, &index); // #9188 L
    contract::settle_no_survivors(result, inputs.wiped_model_verdict.as_ref()); // #9188 A, J
    // #9310: the withheld mapping, last, from the reviewer's own verdict.
    apply_withheld_outcome(result, &inputs.judged.verdict, error_before);
    apply_grade_floor(result, &inputs.judged.grade_floor); // #9310 ruling 50: last, so nothing relaxes it
    contract::write_summary(result, narrative); // #9188 C; #9310 D2: always the template
    contract::prepend_withheld_headline(result); // #9310: one total, from the array
}

/// The #8904 verifier round on the gate's survivors, and its withhold policy.
async fn verify_survivors(
    config: &ReviewConfig,
    verifier: Option<&Arc<dyn LlmProvider>>,
    result: &mut ReviewResult,
    inputs: &GateInputs<'_>,
) {
    let primary = result.verdict.clone();
    let Some(report) = maybe_verify(
        config,
        verifier,
        inputs.diff,
        inputs.per_file,
        primary,
        &mut result.findings,
        inputs.author_rationale,
    )
    .await
    else {
        withhold_unverified(result, config.verification.enabled);
        return;
    };
    result.verdict = report.verdict.clone();
    // #4044: a withheld UNVERIFIABLE was never confirmed either.
    result.withheld_unverified_count = report.unjudged + report.over_cap + report.unverifiable;
    result
        .withheld_findings
        .extend(report.withheld_findings.iter().cloned());
    let Some(note) = report.note() else {
        return;
    };
    result.review_body = withhold::scrub_body(&result.review_body, &report.withheld);
    // #9310: no per-stage headline; `prepend_withheld_headline` counts the array.
    if result.verdict == Verdict::Unknown {
        result.grade = None;
        result.error.get_or_insert(note);
    }
}

/// Why verification is enabled yet no verifier is wired: its build failed.
const NO_VERIFIER: &str = "no verifier provider could be built";

/// Why no round ran when `[verification] enabled` is false.
const VERIFICATION_DISABLED: &str = "verification is disabled";

/// `WithheldFinding::reason` for a finding no verifier round checked.
pub const NO_VERIFIER_REASON: &str = "no verifier";

/// Withhold every finding when no verification round ran (#4044).
///
/// Why: Bob's ruling of 2026-09-30 ("withhold all") supersedes the 09-29
/// #8904 rule that posted unchecked findings behind a note: only a CONFIRMED
/// finding is posted, so with no round nothing is.
/// What: no findings → nothing to do, verdict untouched. Otherwise moves every
/// finding into `result.withheld_findings` with reason [`NO_VERIFIER_REASON`],
/// keeps "N findings withheld: no verifier (<why>)" as the error note (the
/// body headline is written once, #9310), and settles the
/// verdict with `settle_withheld` and no survivors: APPROVE / APPROVE* keeps
/// its verdict and exits 0 (AQ-7t, Bob 2026-10-05); any other verdict is
/// `Unknown` with no grade and the note as the error, so no blocking verdict
/// rests on findings nobody checked. With verification enabled but no verifier built,
/// each finding is first marked `Unverifiable` and counted in
/// `withheld_unverified_count` (the #4459 alarm); with verification disabled by
/// config, an operator choice, they keep no outcome and are not counted.
/// Test: `run_review_enabled_without_a_verifier_withholds_every_finding`,
/// `run_review_disabled_verification_withholds_every_finding`,
/// `run_review_no_verifier_all_withheld_approve_stays_approve_and_exits_zero`.
fn withhold_unverified(result: &mut ReviewResult, enabled: bool) {
    if result.findings.is_empty() {
        return;
    }
    let count = result.findings.len();
    let why = if enabled {
        mark_unverifiable(&mut result.findings, NO_VERIFIER);
        result.withheld_unverified_count = count;
        NO_VERIFIER
    } else {
        VERIFICATION_DISABLED
    };
    let mut cites = Vec::new();
    for f in std::mem::take(&mut result.findings) {
        warn!(file = %f.file, line = ?f.line, kind = %f.kind, why, "verifier: withholding unchecked finding (#4044)");
        if let Some(line) = f.line {
            cites.push(format!("{}:{line}", f.file));
        }
        result.withheld_findings.push(WithheldFinding {
            finding: f,
            reason: NO_VERIFIER_REASON.to_string(),
            missing_fragment: None,
        });
    }
    let note = format!("{count} findings withheld: no verifier ({why})");
    result.review_body = withhold::scrub_body(&result.review_body, &cites);
    // #9310: no per-stage headline; `prepend_withheld_headline` counts the array.
    // AQ-7t (Bob 2026-10-05): keep APPROVE, as on the verifier path.
    result.verdict = withhold::settle_withheld(result.verdict.clone(), &[]);
    if result.verdict == Verdict::Unknown {
        result.grade = None;
        result.error.get_or_insert(note);
    }
}

/// Mark every finding with no recorded outcome `Unverifiable` (#8904).
///
/// Why: a finding no verifier could check was never checked, and
/// `count_unverified` counts only a recorded outcome.
/// What: sets `verified` directly, skipping `apply_outcome`'s #5309 demotion.
/// A finding a hygiene pass already stamped keeps its own outcome and reason.
/// Test: `run_review_enabled_without_a_verifier_withholds_every_finding`.
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
