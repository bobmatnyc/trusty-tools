//! Output-hygiene filters applied to LLM-authored findings before grading and
//! rendering (#4043, #4044).
//!
//! Why: both defects share the shape this module closes — the reviewer's OWN
//! text already disqualifies the finding (it withdrew the claim, it leaked raw
//! mid-reasoning deliberation, or it admits it is speculating beyond what the
//! diff actually shows), yet nothing acted on that self-admission before the
//! finding reached the verdict floor or the rendered review:
//!
//!  - **#4044, defect 1 — unsound verdict.** A review graded F/BLOCK with 7 of
//!    34 findings self-withdrawn in their own published text ("Withdrawing this
//!    finding — confidence too low.", "This is correctly implemented."). The
//!    aggregator counted cardinality/severity of EMITTED findings, never
//!    checking whether the finding's own prose had already negated itself.
//!  - **#4044, defect 2 — chain-of-thought leak.** One finding's published body
//!    included raw deliberation verbatim ("Wait — actually looking at the code
//!    more carefully, ..."). There is no separate "thinking" channel in this
//!    pipeline (see `llm::bedrock` — no extended-thinking mode is requested);
//!    the model narrates reasoning directly inside the same `body`/`description`
//!    field the JSON schema exposes, so a leak is an output-text problem, not a
//!    transport one.
//!  - **#4043 — spec reported as implementation.** A finding derived from a
//!    SPEC/doc artifact was emitted as if it described shipped code, refuted by
//!    the actual implementation. The clearest instances are findings that
//!    ADMIT the gap in their own text ("the diff contains only the spec
//!    document ... no implementation ... is present", "if implementation
//!    follows the interface literally") and reason past it anyway.
//!
//! What: [`sanitize_findings`] runs three independent, marker-based passes, all
//! reusing the SAME shape — the finding's own text is the signal, no LLM
//! double-check required:
//!
//!  1. [`drop_self_negated_or_leaked_findings`] — DROPS any finding whose text
//!     contains a self-negation marker ("withdrawing", "this is correct", …) or
//!     a raw-deliberation transition marker ("wait — actually", "on
//!     re-inspection", …). Dropping (not stripping-in-place) is deliberate:
//!     Bob's simplify mandate prefers a finding that cannot leak at all over a
//!     surgical text edit that risks leaving a mangled remainder; a finding
//!     that talked itself out of its own claim is not lost signal.
//!  2. [`demote_diff_absent_speculation`] — DEMOTES (does not drop — the
//!     documentation-drift signal is real and worth keeping, per #4043's own
//!     suggested direction) any High/Critical finding whose text admits it is
//!     reasoning about an implementation that is not present in the diff. This
//!     structurally prevents it from ever driving BLOCK
//!     (`grade::drives_block_floor` requires `Effort::High`), while leaving it
//!     as an advisory note.
//!  3. [`claim_grounding::demote_ungrounded_registry_claims`] (#4081) — DEMOTES
//!     (does not drop) any finding asserting a package-registry fact nothing in
//!     this pipeline looked up, and pre-stamps it `Unverifiable` so the verifier
//!     round can never mark it `Confirmed`. Same defect family as the two above —
//!     the system asserting a state it never observed — but a distinct signal
//!     (registry vocabulary, not self-admission), so it lives in its own module
//!     with its own marker sets and test surface, exactly as `citation_check`
//!     does. See `claim_grounding` for the full rationale.
//!
//! [`claim_grounding::demote_ungrounded_registry_claims`]:
//!     crate::pipeline::claim_grounding::demote_ungrounded_registry_claims
//!
//! Both passes run BEFORE `citation_check::enforce_citation_integrity` (see
//! `runner.rs` / `mapreduce/mod.rs`) so a self-negated finding never
//! participates in the mutual-contradiction check either.
//!
//! Test: `finding_hygiene_tests.rs`.

use tracing::warn;

use crate::models::{Effort, Finding, Verdict};

/// Case-insensitive substrings that mean the finding's OWN text has already
/// negated it, or leaked raw mid-reasoning deliberation (#4044).
///
/// Why: chosen from the VERBATIM closing lines and transition phrases #4044
/// quotes (`"Withdrawing this finding — confidence too low."`, `"This is
/// correctly implemented."`, `"Wait — actually looking at the code more
/// carefully"`, `"On re-inspection this is actually correct"`) — each is a
/// phrase a model uses specifically when narrating that IT no longer believes
/// its own claim, or when the raw deliberation trace itself leaked into the
/// field. Deliberately NOT single generic words ("correct", "issue") that
/// would false-drop a legitimate finding describing correct behavior elsewhere
/// in the same file.
/// What: matched via `str::contains` after lowercasing both the marker and the
/// finding's text (cheap, dependency-free, no regex needed for fixed
/// substrings).
/// Test: `drops_finding_containing_each_self_negation_marker`,
/// `does_not_drop_legitimate_finding_using_the_word_correct`.
const SELF_NEGATION_MARKERS: &[&str] = &[
    "withdrawing",
    "withdraw this finding",
    "not a finding",
    "no finding here",
    "no actual bug",
    "this is a non-issue",
    "is correctly implemented",
    "the code is correct",
    "appears correct",
    "actually correct",
    "wait — actually",
    "wait, actually",
    "wait -- actually",
    "actually looking at the code more carefully",
    "on re-inspection",
    "on second look, this is",
    "let me re-check",
    "let me reconsider",
];

/// Case-insensitive substrings meaning the finding's own text admits it is
/// speculating about an implementation that is NOT present in the diff under
/// review (#4043).
///
/// Why: chosen from the VERBATIM admissions #4043 quotes ("the diff contains
/// only the spec document ... no implementation ... is present", "If
/// implementation follows the interface literally", "An implementer following
/// §5 literally would introduce", "this is a spec document (not yet
/// implemented code)"). Each phrase is the model narrating the SAME
/// stop-condition #4043 asks be enforced: when the implementation is not in
/// the diff, reasoning about what it "would" do is speculation, not a code
/// finding.
/// What: matched via `str::contains` after lowercasing.
/// Test: `demotes_finding_admitting_diff_absent_speculation`.
const DIFF_ABSENT_SPECULATION_MARKERS: &[&str] = &[
    "no implementation of",
    "if implementation follows",
    "if an implementer follows",
    "an implementer following",
    "not yet implemented",
    "is a spec document",
    "is not yet implemented code",
];

/// Counts returned by [`sanitize_findings`], one per pass.
///
/// Why: a bare tuple stopped reading clearly once a third pass (#4081) joined
/// the two original ones — `(usize, usize, usize)` at a call site says nothing
/// about which count is which.
/// What: plain public counters; every field is "how many findings this pass
/// acted on".
/// Test: `sanitize_findings_runs_every_pass`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HygieneCounts {
    /// Findings dropped as self-negated / chain-of-thought-leaking (#4044).
    pub dropped_self_negated: usize,
    /// Findings demoted as diff-absent speculation (#4043).
    pub demoted_diff_absent: usize,
    /// Findings demoted as unverified package-registry claims (#4081).
    pub demoted_ungrounded_registry: usize,
}

/// Run every output-hygiene pass over `findings`, in place.
///
/// Why: a single call site for #4043, #4044 and #4081's fixes keeps `runner.rs`
/// and `mapreduce/mod.rs` from having to know the internal pass ordering.
/// What: drops self-negated/leaked findings FIRST (so they never participate
/// in downstream checks — e.g. `citation_check`'s mutual-contradiction pass),
/// then demotes any surviving diff-absent-speculation High finding, then demotes
/// any surviving unverified package-registry claim (`claim_grounding`, #4081 —
/// last, because the advisory note it appends contains registry vocabulary the
/// earlier marker passes have no business re-reading).
/// Test: `sanitize_findings_runs_every_pass`.
pub fn sanitize_findings(findings: &mut Vec<Finding>) -> HygieneCounts {
    HygieneCounts {
        dropped_self_negated: drop_self_negated_or_leaked_findings(findings),
        demoted_diff_absent: demote_diff_absent_speculation(findings),
        demoted_ungrounded_registry:
            crate::pipeline::claim_grounding::demote_ungrounded_registry_claims(findings),
    }
}

/// Drop any finding whose `kind`/`description`/`consequence` contains a
/// self-negation or raw-deliberation marker (#4044).
///
/// Why: a finding that withdrew itself, or that leaked its own mid-reasoning
/// trace, must never reach the rendered review or the verdict floor. Dropping
/// the WHOLE finding (rather than truncating the text at the marker) is the
/// structurally simple choice: it cannot leave a half-scrubbed remainder, and
/// a finding contaminated enough to narrate its own uncertainty this openly is
/// not reliable signal worth salvaging.
/// What: case-insensitive substring match against [`SELF_NEGATION_MARKERS`]
/// over `kind`, `description`, and `consequence`; a hit removes the finding
/// from the `Vec` and logs the reason at `warn` (never surfaced to the
/// rendered review — this IS the mechanism that keeps it from being surfaced).
/// Test: `drops_finding_containing_each_self_negation_marker`,
/// `does_not_drop_legitimate_finding_using_the_word_correct`.
pub fn drop_self_negated_or_leaked_findings(findings: &mut Vec<Finding>) -> usize {
    let mut dropped = 0usize;
    let mut kept = Vec::with_capacity(findings.len());
    for f in std::mem::take(findings) {
        match matching_marker(&f, SELF_NEGATION_MARKERS) {
            Some(marker) => {
                warn!(
                    file = %f.file,
                    line = ?f.line,
                    kind = %f.kind,
                    marker,
                    "finding-hygiene: dropping self-negated/CoT-leaking finding (#4044)"
                );
                dropped += 1;
            }
            None => kept.push(f),
        }
    }
    *findings = kept;
    dropped
}

/// Demote (never drop) any High/Critical finding admitting it reasons about an
/// implementation absent from the diff (#4043).
///
/// Why: #4043's own suggested direction — "reclassify spec/code disagreement
/// as documentation drift ... severity-floored and excluded from a BLOCK
/// verdict" — asks for demotion, not deletion: the observation that a spec and
/// the (absent) implementation disagree is real and useful, it is simply not a
/// CODE finding. Capping `effort` at `Medium` structurally excludes it from
/// `grade::drives_block_floor` (which requires `Effort::High`) without a
/// second escalation-eligibility check.
/// What: for each finding whose `description`/`consequence` matches
/// [`DIFF_ABSENT_SPECULATION_MARKERS`] AND whose `effort` is `High`, sets
/// `effort = Medium` and `code_provable = false` (a claim about an absent
/// implementation cannot be "provable from the diff itself"). Findings already
/// below `High` are left untouched — they cannot drive BLOCK regardless.
/// Test: `demotes_finding_admitting_diff_absent_speculation`,
/// `does_not_touch_medium_diff_absent_finding`.
pub fn demote_diff_absent_speculation(findings: &mut [Finding]) -> usize {
    let mut demoted = 0usize;
    for f in findings.iter_mut() {
        if f.effort != Effort::High {
            continue;
        }
        if let Some(marker) = matching_marker(f, DIFF_ABSENT_SPECULATION_MARKERS) {
            warn!(
                file = %f.file,
                line = ?f.line,
                kind = %f.kind,
                marker,
                "finding-hygiene: demoting diff-absent speculation to advisory (#4043)"
            );
            f.effort = Effort::Medium;
            f.code_provable = false;
            demoted += 1;
        }
    }
    demoted
}

/// Relax a self-reported `verdict`/`grade` to APPROVE when THIS run's hygiene +
/// citation-integrity passes wiped out every finding that used to be present
/// (#4042, #4044).
///
/// Why: `grade::derive_verdict` takes `stricter_of(model_proposed, floor)` —
/// by design it can only RAISE a verdict via the floor, never lower the
/// model's own raw self-reported `verdict`/`grade` JSON fields (that
/// asymmetry is deliberate elsewhere — see `verify::rederive_verdict`'s
/// path (c), which preserves a self-reported verdict across a verifier
/// INFRASTRUCTURE failure, #726). Dropping fabricated/self-negated findings
/// therefore closes the FLOOR half of "fabricated findings block clean PRs",
/// but not the other half: the SAME hallucinating LLM call that invented the
/// findings also wrote the top-level `verdict: "BLOCK"` / `grade: "F"`
/// fields, and those are not automatically corrected by pruning the finding
/// list underneath them.
///
/// This closes that gap WITHOUT touching `derive_verdict`'s shared,
/// heavily-calibrated floor semantics (which every other caller —
/// `verify::rederive_verdict`, `mapreduce::reduce`, the synthesis floor —
/// continues to rely on unchanged): it is gated on a LOCALLY OBSERVED fact
/// this run (findings went from non-empty to empty because OUR OWN passes
/// just removed them), not on "findings happens to be empty" in general,
/// so it can never fire on `verify::rederive_verdict`'s infra-failure path
/// (a completely different call site, operating on already-hygiene-passed
/// findings).
/// What: when `findings_before > 0`, `findings.is_empty()`, and `verdict` is
/// neither already `Approve` nor the terminal `Unknown`, resets `verdict` to
/// `Approve` and `grade` to `None` (letting the caller's existing
/// `default_grade_for_verdict` fallback recompute a grade consistent with
/// APPROVE) and logs why. No-op otherwise.
/// Test: `relaxes_verdict_when_all_findings_wiped_this_run`,
/// `does_not_relax_when_findings_were_already_empty`,
/// `does_not_relax_when_findings_survive`.
pub fn relax_verdict_if_evidence_wiped(
    verdict: &mut Verdict,
    grade: &mut Option<String>,
    findings_before: usize,
    findings: &[Finding],
) {
    if findings_before == 0 || !findings.is_empty() {
        return;
    }
    if *verdict == Verdict::Approve || *verdict == Verdict::Unknown {
        return;
    }
    warn!(
        prior_findings = findings_before,
        verdict = %verdict,
        "finding-hygiene: every finding was dropped this run — the model's own \
         top-level verdict/grade rested on the same evidence and cannot be \
         trusted either; relaxing to APPROVE (#4042, #4044)"
    );
    *verdict = Verdict::Approve;
    *grade = None;
}

/// Return the first marker from `markers` found (case-insensitively) in
/// `f.kind`, `f.description`, or `f.consequence`, if any.
fn matching_marker(f: &Finding, markers: &[&'static str]) -> Option<&'static str> {
    let haystack = format!("{} {} {}", f.kind, f.description, f.consequence).to_lowercase();
    markers
        .iter()
        .find(|marker| haystack.contains(*marker))
        .copied()
}

#[cfg(test)]
#[path = "finding_hygiene_tests.rs"]
mod tests;
