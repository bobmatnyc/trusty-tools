//! Evidence-admission guard: a finding that says it cannot be checked must not
//! be marked `verified: "confirmed"` (#5309).
//!
//! Why: #4081 established the rule — *a claim may be marked `confirmed` only
//! when something actually verified it* — and PR #4212 encoded it in
//! `claim_grounding`. That encoding keys on the claim's **subject**: registry
//! vocabulary (`PyPI`, `requirements.txt`, …) paired with a registry-state
//! assertion. A subject-keyed classifier only ever covers the subjects someone
//! has already been burned by, so the same defect recurred on the next surface.
//!
//! #5309 is that recurrence, on an in-repo function signature. `review_pr`
//! returned BLOCK/F on a finding with `confidence: 0.72`, `verified:
//! "confirmed"`, `code_provable: true`, claiming four functions were called
//! without `.await`. Its own description said:
//!
//! > "The diff does not show their signatures, so this cannot be confirmed from
//! > the diff alone."
//!
//! The four functions are synchronous `pub fn`s; an un-awaited future would not
//! typecheck. Nothing checked the signatures — the verifier LLM was handed the
//! same diff the finding had already declared insufficient, and its schema gave
//! it only CONFIRMED or REFUTED to answer with.
//!
//! ## What: key on the admission, not on the subject
//!
//! This module generalises #4212's rule one step: it keys on the finding's own
//! **epistemic admission** — "cannot be confirmed from the diff", "the diff does
//! not show", "without seeing the …" — which is subject-agnostic by
//! construction. A signature claim, a registry claim, a caller-behaviour claim
//! and any future surface all trip the same marker set the moment the finding
//! admits its evidence is missing.
//!
//! [`demote_self_admitted_unverifiable`] pre-stamps each such finding
//! `VerifyOutcome::Unverifiable` BEFORE the verification round, and
//! `verify::select_candidates` skips a finding whose outcome is already decided —
//! so the verifier is never asked a question the finding itself said the diff
//! cannot answer, and can never stamp `Confirmed` over it. That is #4212's
//! mechanism, reused rather than duplicated.
//!
//! ## The residual, stated plainly
//!
//! This does not cover a finding that is unverifiable but does not say so. Two
//! things narrow that gap without a symbol lookup:
//!
//!  - `verify_prompt` now offers the verifier a third judgment, `UNVERIFIABLE`,
//!    so a finding it cannot check from the diff is recorded as unchecked
//!    instead of being forced into a binary CONFIRMED/REFUTED choice.
//!  - [`demote_to_unverifiable_advisory`] is the single demotion both this pass
//!    and `verify::apply_outcome` apply, so an `Unverifiable` finding carries
//!    the same non-escalating weight however it was classified.
//!
//! Resolving the claimed symbol for real — reading `pub fn run` out of the
//! repository — is a different change: the verification stage receives only the
//! verifier provider and the diff string, so it would mean threading a
//! `SearchClient` through `maybe_verify` / `run_verification_round` (both public)
//! and adding a symbol-resolution API, then parsing a natural-language claim into
//! a symbol query. See #5309 for that follow-up.
//!
//! Test: `evidence_admission_tests.rs` (unit, #5309's finding verbatim) and
//! `run_review_self_admitted_unverifiable_claim_is_not_confirmed`
//! (runner_tests.rs — end-to-end through `run_review`).

use tracing::warn;

use crate::models::{Effort, Finding, VerifyOutcome};

/// Confidence an unverifiable finding is capped at.
///
/// Why: the value is load-bearing against two thresholds in `grade.rs` and sits
/// in the only band that satisfies both — it must be at or below
/// `LOW_CONFIDENCE_THRESHOLD` (0.65), the advisory-batch collapse line that
/// dissolves the model's OWN self-reported BLOCK, and at or above
/// `constants::FIX_ISSUE_MIN_CONFIDENCE` (0.60), the documented review-include
/// gate, so the finding stays visible. `claim_grounding` established the value
/// for #4081; this module owns it now so the two passes cannot drift apart.
/// What: applied with `min`, never `max` — a ceiling, not an assignment.
/// Test: `advisory_confidence_sits_in_the_visible_but_non_escalating_band`
/// (claim_grounding_tests.rs).
pub const UNVERIFIABLE_ADVISORY_CONFIDENCE: f32 = 0.65;

/// Reason recorded in [`VerifyOutcome::Unverifiable`] for a self-admitted claim.
const SELF_ADMITTED_REASON: &str = "the finding's own text states it cannot be confirmed from the evidence available to \
     this pipeline";

/// Sentinel opening the advisory note appended to a demoted finding.
///
/// Why: the note itself contains admission vocabulary, so re-running the pass
/// over an already-demoted finding would append it again. Matching on this
/// sentinel makes the pass idempotent by construction rather than by call-site
/// discipline — the same guard `claim_grounding` uses.
/// What: the literal prefix of [`SELF_ADMITTED_NOTE`].
/// Test: `pass_is_idempotent`.
const SELF_ADMITTED_NOTE_SENTINEL: &str = "_Unchecked claim (#5309)";

/// Advisory note appended to the `description` of a demoted finding.
///
/// Why: demoting silently leaves the author reading a confident-sounding claim
/// with no hint that nothing checked it.
/// What: phrased to avoid every marker string in this module's
/// [`SELF_ADMITTED_UNVERIFIABLE_MARKERS`], in `finding_hygiene`'s
/// `SELF_NEGATION_MARKERS` / `DIFF_ABSENT_SPECULATION_MARKERS`, and in
/// `claim_grounding`'s `REGISTRY_SUBJECT_MARKERS`, so appending it can never
/// cause a later pass to drop or re-demote the finding it annotates.
/// Test: `advisory_note_does_not_trip_sibling_pass_markers`.
const SELF_ADMITTED_NOTE: &str = "_Unchecked claim (#5309): this finding states in its own \
     text that the evidence needed to settle it was not available. Nothing in \
     this pipeline read that evidence either, so the claim is reported as \
     unchecked rather than confirmed. Check it directly before acting. Demoted \
     to advisory: it cannot drive a blocking verdict._";

/// Case-insensitive substrings in which a finding admits the evidence it needs
/// is absent from what it was shown.
///
/// Why: every entry is the model narrating a stop-condition about its OWN
/// epistemic position, not about a subject domain — which is what makes the
/// guard general where `claim_grounding`'s registry vocabulary is not. The first
/// two are verbatim from #5309's finding ("The diff does not show their
/// signatures, so this cannot be confirmed from the diff alone").
///
/// Deliberately NOT included: bare "cannot be confirmed" or "unverified", which
/// a legitimate finding uses about the CODE's behaviour ("the response is
/// unverified before use") rather than about its own evidence.
/// What: matched via `str::contains` after lowercasing the concatenation of
/// `kind`, `description`, and `consequence`.
/// Test: `demotes_each_admission_marker`,
/// `does_not_demote_finding_describing_unverified_code`.
const SELF_ADMITTED_UNVERIFIABLE_MARKERS: &[&str] = &[
    "cannot be confirmed from the diff",
    "does not show their signatures",
    "cannot be confirmed from the",
    "can't be confirmed from the",
    "cannot be verified from the diff",
    "cannot be verified from the",
    "from the diff alone",
    "the diff does not show",
    "the diff doesn't show",
    "not shown in the diff",
    "not visible in the diff",
    "is not included in the diff",
    "are not included in the diff",
    "cannot be determined from the diff",
    "cannot tell from the diff",
    "without seeing the",
    "i cannot verify",
    "i can't verify",
    "i am unable to verify",
    "unable to verify this",
    "cannot confirm whether",
    "could not confirm whether",
    "would need to check the",
    "would need to inspect the",
];

/// Return the admission marker that makes `f` self-admittedly unverifiable, if
/// it is one.
///
/// Why: exposed rather than folded into the demotion loop so the classification
/// can be asserted directly in tests and reused by a caller that needs to ask
/// "did this finding admit it could not check itself?" without mutating anything.
/// What: returns the first [`SELF_ADMITTED_UNVERIFIABLE_MARKERS`] entry found in
/// the lowercased concatenation of `kind`, `description`, and `consequence`.
/// Test: `demotes_the_verbatim_5309_finding`,
/// `does_not_demote_finding_describing_unverified_code`.
pub fn self_admission_marker(f: &Finding) -> Option<&'static str> {
    let haystack = format!("{} {} {}", f.kind, f.description, f.consequence).to_lowercase();
    SELF_ADMITTED_UNVERIFIABLE_MARKERS
        .iter()
        .find(|m| haystack.contains(*m))
        .copied()
}

/// Strip a finding of every signal that lets it escalate a verdict.
///
/// Why: an `Unverifiable` finding reaches that state by two routes — pre-stamped
/// by a hygiene pass ([`demote_self_admitted_unverifiable`],
/// `claim_grounding::demote_ungrounded_registry_claims`) or returned by the
/// verifier itself (`verify::apply_outcome`). Both must leave the finding with
/// the same weight, or the same claim escalates or not depending on which route
/// classified it. One function, three call sites.
/// What: clears `code_provable`, caps `effort` at `Medium`, and caps
/// `confidence` at [`UNVERIFIABLE_ADVISORY_CONFIDENCE`]. Together these drop the
/// finding out of `grade::drives_block_floor` (which needs `Effort::High` AND a
/// citation or `code_provable`) and out of `grade::is_substantive`'s
/// high-severity retention path, while leaving it visible in the rendered
/// review. Does NOT set `verified` — the caller records the reason it knows.
/// Test: `demote_is_a_ceiling_never_a_promotion`,
/// `demoted_finding_does_not_drive_the_block_floor`.
pub fn demote_to_unverifiable_advisory(f: &mut Finding) {
    f.code_provable = false;
    if f.effort == Effort::High {
        f.effort = Effort::Medium;
    }
    f.confidence = f.confidence.min(UNVERIFIABLE_ADVISORY_CONFIDENCE);
}

/// Demote every finding whose own text admits it could not be checked (#5309).
///
/// Why: see the module doc — a claim nothing verified may not wear the
/// pipeline's verification signals nor drive a blocking verdict, but it must
/// still reach the author. "trusty-review could not settle this" is real
/// information; over-suppression would trade a false BLOCK for a false silence.
/// What: for each finding [`self_admission_marker`] identifies and that has not
/// already been demoted (see [`SELF_ADMITTED_NOTE_SENTINEL`]): stamps
/// `verified = Unverifiable` so `verify::select_candidates` skips it, applies
/// [`demote_to_unverifiable_advisory`], and appends [`SELF_ADMITTED_NOTE`].
/// Returns how many were demoted. Never drops a finding, and never touches a
/// finding that already carries a decided `verified` outcome.
/// Test: `demotes_the_verbatim_5309_finding`, `pass_is_idempotent`,
/// `does_not_demote_finding_describing_unverified_code`.
pub fn demote_self_admitted_unverifiable(findings: &mut [Finding]) -> usize {
    let mut demoted = 0usize;
    for f in findings.iter_mut() {
        if f.verified.is_some() || f.description.contains(SELF_ADMITTED_NOTE_SENTINEL) {
            continue;
        }
        let Some(marker) = self_admission_marker(f) else {
            continue;
        };
        warn!(
            file = %f.file,
            line = ?f.line,
            kind = %f.kind,
            marker,
            prior_confidence = f.confidence,
            prior_effort = %f.effort,
            prior_code_provable = f.code_provable,
            "evidence-admission: finding says it could not be checked — demoting to advisory (#5309)"
        );
        f.verified = Some(VerifyOutcome::Unverifiable {
            reason: SELF_ADMITTED_REASON.to_string(),
        });
        demote_to_unverifiable_advisory(f);
        f.description = format!("{}\n\n{}", f.description, SELF_ADMITTED_NOTE);
        demoted += 1;
    }
    demoted
}

#[cfg(test)]
#[path = "evidence_admission_tests.rs"]
mod tests;
