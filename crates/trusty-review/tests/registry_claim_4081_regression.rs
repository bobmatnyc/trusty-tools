//! Regression tests for #4081: a hallucinated dependency-version finding wore
//! `code_provable: true` + `verified: "confirmed"` and drove `BLOCK` / `F`.
//!
//! The fixture is the reported case verbatim — `duettoresearch/APEX` PR 1717's
//! one-line CI change (`ruff>=0.6.0` → `ruff==0.16.0`) and the finding text
//! #4081 quotes in full. `ruff==0.16.0` was in fact the CURRENT latest release,
//! and all ten checks on that PR were green on that exact dependency; the claim
//! came from the reviewer model's training-cutoff recollection and nothing in
//! the pipeline looked it up.
//!
//! Three things are locked in:
//!  (a) the claim is NOT marked `verified: "confirmed"` and does NOT drive a
//!      BLOCK — including the model's own self-reported `verdict: "BLOCK"`,
//!      which rested on the same unchecked evidence;
//!  (b) it still SURFACES, as an advisory carrying the model's original prose
//!      plus a note saying nothing checked it — the anti-over-suppression half,
//!      asserted as PRESENCE, not absence;
//!  (c) a finding that IS backed by verification still confirms and still drives
//!      BLOCK — the capability is retained, not traded away for (a).
//!
//! Verified failing before the fix: with `demote_ungrounded_registry_claims`
//! neutered to a no-op, four of the five tests fail — the verdict stays BLOCK,
//! the claim is still selected as a verification candidate, and no advisory note
//! reaches the reader. Only (c), the control, passes in both states, which is
//! exactly what makes it a control.

use trusty_review::models::{Effort, Finding, Verdict, VerifyOutcome};
use trusty_review::pipeline::derive_verdict;
use trusty_review::pipeline::finding_hygiene::sanitize_findings;
use trusty_review::pipeline::select_candidates;

/// The finding #4081 reports, field for field (its JSON is quoted verbatim in
/// the issue's "Actual output" section).
fn ruff_version_finding() -> Finding {
    let mut f = Finding::new(
        ".github/workflows/repos-all-advisory.yml",
        "ruff pinned to non-existent version 0.16.0",
        "The CI workflow pins `ruff==0.16.0`, but ruff's versioning scheme uses a \
         `0.x.y` format where the latest stable releases are in the `0.4.x`–`0.9.x` \
         range as of mid-2026. Version `0.16.0` does not exist on PyPI. This will \
         cause the `pip install` step to fail with a \"No matching distribution \
         found\" error, breaking CI for every PR that touches the linted files.",
        "Pin ruff to a version that exists on PyPI.",
        0.82,
        Effort::High,
    );
    f.line = Some(41);
    f.consequence = "CI pip install step fails with 'No matching distribution found', \
                     breaking the advisory workflow on every run"
        .to_string();
    f.code_provable = true;
    f
}

/// A genuine, diff-provable correctness finding on the same PR — the control.
fn genuine_finding() -> Finding {
    let mut f = Finding::new(
        "src/apex/loader.py",
        "logic-error",
        "`load_config` indexes `argv[1]` before checking `len(argv)`, so running \
         the loader with no arguments raises IndexError instead of printing usage.",
        "Guard the length before indexing.",
        0.9,
        Effort::High,
    );
    f.line = Some(12);
    f.code_provable = true;
    f
}

// ─── (a) the false BLOCK ─────────────────────────────────────────────────────

#[test]
fn ungrounded_version_claim_is_not_confirmed_and_does_not_block() {
    let mut findings = vec![ruff_version_finding()];

    // Precondition — this is exactly what #4081 reports: on unmodified code the
    // claim's self-declared `code_provable: true` + High effort hard-clamps the
    // deterministic floor to BLOCK, from a clean APPROVE baseline.
    assert_eq!(
        derive_verdict(Verdict::Approve, &findings),
        Verdict::Block,
        "precondition: the ungrounded claim forces BLOCK before the guard runs"
    );

    let demoted = sanitize_findings(&mut findings).demoted_ungrounded_registry;
    assert_eq!(demoted, 1, "the registry claim must be demoted");

    // The trust signals are withdrawn...
    let f = &findings[0];
    assert!(
        !f.code_provable,
        "a claim nothing looked up is not provable from the diff"
    );
    assert!(
        !matches!(f.verified, Some(VerifyOutcome::Confirmed)),
        "must never be marked confirmed, got {:?}",
        f.verified
    );
    assert!(
        matches!(f.verified, Some(VerifyOutcome::Unverifiable { .. })),
        "must record WHY it is unverifiable, got {:?}",
        f.verified
    );

    // ...and it can no longer drive a blocking verdict from either direction:
    // not via the deterministic floor...
    assert_eq!(
        derive_verdict(Verdict::Approve, &findings),
        Verdict::Approve,
        "the demoted claim must not floor the verdict"
    );
    // ...and not via the model's OWN self-reported BLOCK, which rested on the
    // same unchecked evidence (#4081 reports `verdict: BLOCK`, `grade: F`).
    assert_eq!(
        derive_verdict(Verdict::Block, &findings),
        Verdict::Approve,
        "a self-reported BLOCK resting solely on the unchecked claim must dissolve"
    );
}

#[test]
fn ungrounded_version_claim_is_never_sent_to_the_verifier() {
    // The `verified: "confirmed"` in #4081's output came from the verifier round.
    // The verifier is the same kind of oracle as the reviewer, so asking it a
    // registry question just launders a stale recollection into a confirmation.
    let mut findings = vec![ruff_version_finding()];

    assert_eq!(
        select_candidates(Verdict::Block, &findings),
        vec![0],
        "precondition: on a blocking verdict the wide net picks it up"
    );

    sanitize_findings(&mut findings);

    assert!(
        select_candidates(Verdict::Block, &findings).is_empty(),
        "a claim the pipeline cannot check must not be put to the verifier"
    );
}

// ─── (b) anti-over-suppression ───────────────────────────────────────────────

#[test]
fn ungrounded_version_claim_still_surfaces_as_advisory() {
    let mut findings = vec![ruff_version_finding()];
    sanitize_findings(&mut findings);

    assert_eq!(findings.len(), 1, "the finding must NOT be dropped");
    let f = &findings[0];

    assert_eq!(f.file, ".github/workflows/repos-all-advisory.yml");
    assert_eq!(f.line, Some(41));
    assert_eq!(f.kind, "ruff pinned to non-existent version 0.16.0");
    assert!(
        f.description
            .contains("Version `0.16.0` does not exist on PyPI."),
        "the model's original claim must survive verbatim so the author can judge \
         it themselves: {}",
        f.description
    );
    assert!(
        f.description.contains("#4081"),
        "the reader must be told nothing verified the claim: {}",
        f.description
    );
    assert!(
        f.description.contains("no registry lookup"),
        "the note must name the specific reason: {}",
        f.description
    );
    assert!(
        f.confidence > 0.0,
        "demoted to advisory, not zeroed out: {}",
        f.confidence
    );
}

// ─── (c) the capability is retained ──────────────────────────────────────────

#[test]
fn verification_backed_finding_still_confirms_and_still_blocks() {
    let mut findings = vec![genuine_finding()];

    sanitize_findings(&mut findings);
    assert_eq!(findings.len(), 1, "a genuine finding is untouched");
    assert!(findings[0].code_provable, "keeps its diff-provable flag");
    assert_eq!(findings[0].effort, Effort::High, "keeps its severity");
    assert!(
        findings[0].verified.is_none(),
        "left open for the verifier to decide"
    );

    // It is still put to the verifier...
    assert_eq!(
        select_candidates(Verdict::Block, &findings),
        vec![0],
        "a checkable finding must still be verified"
    );

    // ...and a CONFIRMED outcome still drives BLOCK.
    findings[0].verified = Some(VerifyOutcome::Confirmed);
    assert_eq!(
        derive_verdict(Verdict::Approve, &findings),
        Verdict::Block,
        "a confirmed, diff-provable High finding must still block"
    );
}

#[test]
fn a_genuine_finding_alongside_a_registry_claim_still_blocks() {
    // The demotion must be scoped to the unchecked claim: a real defect found on
    // the SAME PR keeps its full weight, so the guard cannot be used to smuggle a
    // bug past review inside a dependency bump.
    let mut findings = vec![ruff_version_finding(), genuine_finding()];

    let counts = sanitize_findings(&mut findings);
    assert_eq!(
        counts.demoted_ungrounded_registry, 1,
        "only the registry claim"
    );
    assert_eq!(findings.len(), 2, "both findings surface");
    assert_eq!(findings[1].effort, Effort::High);
    assert!(findings[1].code_provable);

    assert_eq!(
        derive_verdict(Verdict::Approve, &findings),
        Verdict::Block,
        "the genuine finding independently drives BLOCK"
    );
}
