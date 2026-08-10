//! Grounding guard for package-registry / version-existence claims (#4081).
//!
//! Why: `review_pr` emitted a `code_provable: true`, `verified: "confirmed"`,
//! `confidence: 0.82` finding asserting that a pinned dependency version does
//! not exist —
//!
//! > "The CI workflow pins `ruff==0.16.0`, but ruff's versioning scheme uses a
//! > `0.x.y` format where the latest stable releases are in the `0.4.x`–`0.9.x`
//! > range as of mid-2026. Version `0.16.0` does not exist on PyPI."
//!
//! — and rolled it up to `verdict: BLOCK` / `grade: F`. `ruff==0.16.0` was the
//! CURRENT latest release, and all ten checks on the PR were green on that exact
//! dependency. The claim came from the reviewer model's training-cutoff
//! recollection; **nothing in this pipeline performs a registry lookup**, so
//! nothing verified it.
//!
//! Two things went wrong, and only the second is fixable here:
//!
//!  1. The model recalled a stale fact. Unfixable by construction — a package
//!     registry moves continuously, and a model's version knowledge is stale the
//!     day it ships.
//!  2. The claim wore the pipeline's two strongest trust signals (`code_provable:
//!     true`, `verified: "confirmed"`) without anything having checked it, and
//!     those signals let it hard-clamp the verdict. **This is the defect.** A
//!     hallucination wearing a confirmation is worse than no finding at all: it
//!     inverts the signal consumers use to decide what to trust.
//!
//! And it lands precisely where it does the most damage. A dependency-bump diff
//! is the single shape where the reviewer's version knowledge is guaranteed
//! stale, and the shape whose author most expects to sail through.
//!
//! ## What: refuse to confirm, do not look up
//!
//! The obvious alternative — actually query PyPI/npm/crates.io — was considered
//! and rejected. It buys nothing the reported harm needs: the false BLOCK is
//! cured by declining to mark an unchecked claim as checked, and a lookup adds a
//! live network dependency, rate limits, per-ecosystem client code, and a fresh
//! way for the review path itself to hang or fail. The general principle this
//! module encodes is narrower and stronger than any lookup:
//!
//! > **A claim may be marked `verified: "confirmed"` only when something actually
//! > verified it.**
//!
//! So [`demote_ungrounded_registry_claims`] finds findings whose own text makes a
//! registry-currency assertion and demotes each to the advisory tier:
//!
//!  - `verified = Unverifiable { reason }` — pre-stamped BEFORE the verification
//!    round, and `verify::select_candidates` skips findings whose outcome is
//!    already decided, so the verifier LLM is never asked a question it cannot
//!    answer and can never stamp `Confirmed` over it.
//!  - `code_provable = false` — a registry fact is by definition not provable
//!    from the diff. This alone drops the finding out of
//!    `grade::drives_block_floor`.
//!  - `effort` capped at `Medium` — the same demotion
//!    `finding_hygiene::demote_diff_absent_speculation` applies (#4043).
//!  - `confidence` capped at [`UNGROUNDED_CLAIM_ADVISORY_CONFIDENCE`] — see that
//!    constant for why the exact value matters to `grade.rs`.
//!  - an advisory note appended to `description`, so a human reading the review
//!    is told the claim is unchecked rather than silently handed a weaker one.
//!
//! ## What it deliberately does NOT do
//!
//! It never drops the finding. "trusty-review could not check this version"
//! is real, useful information for the author — a nudge to confirm the pin
//! themselves. Over-suppression would trade a false BLOCK for a false silence.
//! The finding stays in the rendered review verbatim (plus the note); only its
//! grading weight is removed.
//!
//! Test: `claim_grounding_tests.rs` (unit) and
//! `tests/registry_claim_4081_regression.rs` (the issue's verbatim finding,
//! end-to-end through the grade derivation).

use tracing::warn;

use crate::models::{Finding, VerifyOutcome};
use crate::pipeline::evidence_admission::{
    UNVERIFIABLE_ADVISORY_CONFIDENCE, demote_to_unverifiable_advisory,
};

/// Confidence a demoted ungrounded registry claim is capped at.
///
/// #5309: the value now lives in `evidence_admission`, which owns the
/// demotion every `Unverifiable` route applies; this stays as the name #4081's
/// tests and docs reference. Its rationale, unchanged:
///
/// Why: the value is load-bearing against two thresholds in `grade.rs`, and sits
/// in the only band that satisfies both:
///
///  - It must be `<= grade::LOW_CONFIDENCE_THRESHOLD` (0.65 — "the advisory-batch
///    collapse line"). `derive_verdict_with`'s low-confidence override returns
///    APPROVE when every substantive finding sits at or below that line and none
///    drives the BLOCK floor. That is what dissolves the model's OWN self-reported
///    `verdict: "BLOCK"` — the severity floor alone cannot, because
///    `stricter_of(model_proposed, floor)` can only ever RAISE toward the floor.
///    Reusing the existing override rather than adding a parallel relaxation is
///    deliberate: #4057 established that mechanism-per-defect proliferation is the
///    thing to avoid here.
///  - It must stay high enough to keep the finding VISIBLE. Nothing in the render
///    path filters on confidence today, but `constants::FIX_ISSUE_MIN_CONFIDENCE`
///    (0.60) is the documented review-include gate, so staying at or above it
///    keeps the anti-over-suppression guarantee true even if that gate is wired
///    up later.
///
/// It also lands below `constants::BLOCK_ISSUE_MIN_CONFIDENCE` (0.75), so an
/// unchecked registry claim can never open a tracker issue either.
///
/// What: applied with `min`, never `max` — a finding the model already rated
/// lower keeps its lower value; this is a ceiling, not an assignment.
/// Test: `advisory_confidence_sits_in_the_visible_but_non_escalating_band`.
pub const UNGROUNDED_CLAIM_ADVISORY_CONFIDENCE: f32 = UNVERIFIABLE_ADVISORY_CONFIDENCE;

/// Sentinel opening the advisory note appended to a demoted finding.
///
/// Why: [`demote_ungrounded_registry_claims`] runs once per pipeline path today,
/// but the note it appends itself contains registry vocabulary — re-running the
/// pass over already-demoted findings would otherwise append the note again.
/// Matching on this sentinel makes the pass idempotent by construction rather
/// than by call-site discipline.
/// What: the literal prefix of [`UNGROUNDED_CLAIM_NOTE`].
/// Test: `pass_is_idempotent`.
const UNGROUNDED_CLAIM_NOTE_SENTINEL: &str = "_Unverified registry claim (#4081)";

/// Advisory note appended to the `description` of a demoted finding.
///
/// Why: demoting silently would leave the author reading a confident-sounding
/// claim with no hint that nothing checked it. The note names the specific reason
/// and tells them what to do about it.
/// What: deliberately phrased to avoid every marker string in
/// `finding_hygiene`'s `SELF_NEGATION_MARKERS` / `DIFF_ABSENT_SPECULATION_MARKERS`
/// (no "does not exist", no "not a finding", no "appears correct") so appending it
/// can never cause a later hygiene pass to drop the finding it is annotating.
/// Test: `advisory_note_does_not_trip_finding_hygiene_markers`.
const UNGROUNDED_CLAIM_NOTE: &str = "_Unverified registry claim (#4081): this \
     finding asserts a package-registry fact — that a pinned version is absent \
     from the registry, was withdrawn, or is out of date. trusty-review performs \
     no registry lookup, and a reviewer model's version knowledge is stale by \
     construction, so nothing here checked it. Confirm against the registry \
     directly before acting. Demoted to advisory: it cannot drive a blocking \
     verdict._";

/// Reason recorded in [`VerifyOutcome::Unverifiable`] for a demoted claim.
const UNVERIFIABLE_REASON: &str =
    "package-registry version/deprecation claim; no registry lookup is performed by this pipeline";

/// Case-insensitive substrings naming a package registry, a package manifest, or
/// an install command — the SUBJECT half of a registry claim.
///
/// Why: the assertion half alone ("does not exist", "latest version") is far too
/// generic to key on — plenty of legitimate code findings use those words about
/// a variable, a route, or a config key. Requiring the finding to ALSO be talking
/// about a package registry is what keeps the guard from firing on ordinary
/// correctness findings.
/// What: matched via `str::contains` after lowercasing the finding's combined
/// text. Covers the five registries a reviewer model most often mis-recalls plus
/// the manifests/commands that unambiguously scope a claim to one of them.
/// Test: `does_not_demote_ordinary_finding_using_existence_words`.
const REGISTRY_SUBJECT_MARKERS: &[&str] = &[
    "pypi",
    "npmjs",
    "npm registry",
    "crates.io",
    "rubygems",
    "packagist",
    "maven central",
    "nuget",
    "go module proxy",
    "package registry",
    "package index",
    "pip install",
    "npm install",
    "yarn add",
    "cargo add",
    "requirements.txt",
    "pyproject.toml",
    "package.json",
    "package-lock.json",
    "cargo.toml",
    "go.mod",
    "gemfile",
    "poetry.lock",
    "uv.lock",
];

/// Case-insensitive substrings asserting a registry FACT the model cannot know —
/// the ASSERTION half of a registry claim.
///
/// Why: every entry is a claim about the state of a live, continuously-moving
/// registry at review time (does this version exist / is it the newest / was it
/// withdrawn / is it deprecated), or an explicit appeal to the model's own
/// training cutoff. Those are exactly the assertions a training-cutoff
/// recollection cannot support. The first five are verbatim from #4081's own
/// output ("does not exist on PyPI", "No matching distribution found", "the
/// latest stable releases are ... as of mid-2026").
/// What: matched via `str::contains` after lowercasing, and required to co-occur
/// with a [`REGISTRY_SUBJECT_MARKERS`] hit.
/// Test: `demotes_the_verbatim_4081_finding`, `demotes_each_assertion_marker`.
const REGISTRY_ASSERTION_MARKERS: &[&str] = &[
    "does not exist",
    "doesn't exist",
    "no matching distribution",
    "non-existent",
    "nonexistent",
    "no such version",
    "not published",
    "never published",
    "unpublished",
    "was yanked",
    "has been yanked",
    "is yanked",
    "latest stable",
    "latest release",
    "latest version",
    "most recent release",
    "is deprecated",
    "has been deprecated",
    "deprecated in version",
    "as of my knowledge",
    "as of my training",
    "knowledge cutoff",
];

/// Return the assertion marker that makes `f` an unverifiable registry claim, if
/// it is one.
///
/// Why: exposed (rather than folded into the demotion loop) so the classification
/// can be asserted directly in tests and reused by any future caller that needs
/// to ask "is this a registry claim?" without mutating anything.
/// What: requires BOTH a [`REGISTRY_SUBJECT_MARKERS`] hit and a
/// [`REGISTRY_ASSERTION_MARKERS`] hit in the lowercased concatenation of `kind`,
/// `description`, and `consequence`; returns the assertion marker that matched.
/// Test: `demotes_the_verbatim_4081_finding`,
/// `does_not_demote_ordinary_finding_using_existence_words`,
/// `does_not_demote_registry_mention_without_an_assertion`.
pub fn registry_claim_marker(f: &Finding) -> Option<&'static str> {
    let haystack = format!("{} {} {}", f.kind, f.description, f.consequence).to_lowercase();
    if !REGISTRY_SUBJECT_MARKERS
        .iter()
        .any(|m| haystack.contains(m))
    {
        return None;
    }
    REGISTRY_ASSERTION_MARKERS
        .iter()
        .find(|m| haystack.contains(*m))
        .copied()
}

/// Demote every ungrounded package-registry claim to the advisory tier (#4081).
///
/// Why: see the module doc — a claim nothing verified may not wear the
/// pipeline's verification signals nor drive a blocking verdict, but it must
/// still reach the author.
/// What: for each finding [`registry_claim_marker`] identifies and that has not
/// already been demoted (see [`UNGROUNDED_CLAIM_NOTE_SENTINEL`]): stamps
/// `verified = Unverifiable`, clears `code_provable`, caps `effort` at `Medium`
/// and `confidence` at [`UNGROUNDED_CLAIM_ADVISORY_CONFIDENCE`], and appends
/// [`UNGROUNDED_CLAIM_NOTE`] to the description. Returns how many were demoted.
/// Never drops a finding and never touches a non-registry finding.
/// Test: `demotes_the_verbatim_4081_finding`, `pass_is_idempotent`,
/// `does_not_demote_ordinary_finding_using_existence_words`.
pub fn demote_ungrounded_registry_claims(findings: &mut [Finding]) -> usize {
    let mut demoted = 0usize;
    for f in findings.iter_mut() {
        if f.description.contains(UNGROUNDED_CLAIM_NOTE_SENTINEL) {
            continue;
        }
        let Some(marker) = registry_claim_marker(f) else {
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
            "claim-grounding: demoting unverified package-registry claim to advisory (#4081)"
        );
        f.verified = Some(VerifyOutcome::Unverifiable {
            reason: UNVERIFIABLE_REASON.to_string(),
        });
        // #5309: one demotion for every route to `Unverifiable`, so the same
        // claim carries the same weight whichever pass classified it.
        demote_to_unverifiable_advisory(f);
        f.description = format!("{}\n\n{}", f.description, UNGROUNDED_CLAIM_NOTE);
        demoted += 1;
    }
    demoted
}

#[cfg(test)]
#[path = "claim_grounding_tests.rs"]
mod tests;
