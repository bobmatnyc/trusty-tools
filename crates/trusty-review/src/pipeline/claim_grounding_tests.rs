//! Unit tests for `claim_grounding` (#4081).
//!
//! Fixtures use the VERBATIM finding #4081 reports (the `ruff==0.16.0` claim
//! against `duettoresearch/APEX` PR 1717) so they stay traceable to the reported
//! evidence, plus the negative cases that keep the guard from over-firing.

use super::*;
use crate::config::constants::{BLOCK_ISSUE_MIN_CONFIDENCE, FIX_ISSUE_MIN_CONFIDENCE};
use crate::models::Finding;

/// The finding #4081 reports, field for field.
fn ruff_finding() -> Finding {
    let mut f = Finding::new(
        ".github/workflows/repos-all-advisory.yml",
        "ruff pinned to non-existent version 0.16.0",
        "The CI workflow pins `ruff==0.16.0`, but ruff's versioning scheme uses a \
         `0.x.y` format where the latest stable releases are in the `0.4.x`–`0.9.x` \
         range as of mid-2026. Version `0.16.0` does not exist on PyPI. This will \
         cause the `pip install` step to fail with a \"No matching distribution \
         found\" error, breaking CI for every PR that touches the linted files.",
        "Pin a ruff version that exists.",
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

// ─── The reported defect ─────────────────────────────────────────────────────

#[test]
fn demotes_the_verbatim_4081_finding() {
    let mut findings = vec![ruff_finding()];
    assert_eq!(demote_ungrounded_registry_claims(&mut findings), 1);

    let f = &findings[0];
    assert!(
        !f.code_provable,
        "an unchecked registry fact is not provable from the diff"
    );
    assert_eq!(
        f.effort,
        Effort::Medium,
        "capped out of the BLOCK-floor tier"
    );
    assert!(
        (f.confidence - UNGROUNDED_CLAIM_ADVISORY_CONFIDENCE).abs() < f32::EPSILON,
        "confidence capped at the advisory band, got {}",
        f.confidence
    );
    assert!(
        matches!(f.verified, Some(VerifyOutcome::Unverifiable { .. })),
        "must be pre-stamped Unverifiable, got {:?}",
        f.verified
    );
}

#[test]
fn demoted_finding_still_surfaces_with_an_advisory_note() {
    // Anti-over-suppression: the finding must still be PRESENT and still carry
    // the model's original prose — only its grading weight is removed.
    let mut findings = vec![ruff_finding()];
    demote_ungrounded_registry_claims(&mut findings);

    assert_eq!(findings.len(), 1, "the finding must not be dropped");
    let f = &findings[0];
    assert!(
        f.description
            .contains("Version `0.16.0` does not exist on PyPI."),
        "the original claim text must survive verbatim: {}",
        f.description
    );
    assert!(
        f.description.contains(UNGROUNDED_CLAIM_NOTE_SENTINEL),
        "the reader must be told nothing checked it: {}",
        f.description
    );
    assert!(
        f.confidence >= FIX_ISSUE_MIN_CONFIDENCE,
        "must stay at or above the review-include gate ({FIX_ISSUE_MIN_CONFIDENCE}), \
         got {}",
        f.confidence
    );
}

#[test]
fn demotes_each_assertion_marker() {
    // Every assertion marker, paired with a registry subject, must trip the guard.
    for marker in REGISTRY_ASSERTION_MARKERS {
        let mut findings = vec![Finding::new(
            "requirements.txt",
            "dependency",
            format!("The pinned package in requirements.txt {marker} — check the pin."),
            "fix",
            0.9,
            Effort::High,
        )];
        assert_eq!(
            demote_ungrounded_registry_claims(&mut findings),
            1,
            "assertion marker {marker:?} must be demoted"
        );
    }
}

// ─── Over-firing guards ──────────────────────────────────────────────────────

#[test]
fn does_not_demote_ordinary_finding_using_existence_words() {
    // "does not exist" about a CODE symbol, with no registry subject anywhere —
    // an ordinary correctness finding that must keep its full weight.
    let mut findings = vec![Finding::new(
        "src/router.ts",
        "logic-error",
        "The handler dispatches to `resolveTenant`, but that export does not exist \
         in `src/tenant.ts` — this is a compile error.",
        "Add the export.",
        0.9,
        Effort::High,
    )];
    findings[0].code_provable = true;

    assert_eq!(demote_ungrounded_registry_claims(&mut findings), 0);
    assert_eq!(findings[0].effort, Effort::High);
    assert!(findings[0].code_provable);
    assert!(findings[0].verified.is_none());
}

#[test]
fn does_not_demote_registry_mention_without_an_assertion() {
    // A real, checkable finding ABOUT a manifest file: it argues from the diff,
    // not from a recollection of registry state.
    let mut findings = vec![Finding::new(
        "package.json",
        "build-break",
        "This diff removes `react-dom` from package.json dependencies while \
         `src/index.tsx` still imports it — the build will not resolve.",
        "Restore the dependency.",
        0.88,
        Effort::High,
    )];
    findings[0].code_provable = true;

    assert_eq!(demote_ungrounded_registry_claims(&mut findings), 0);
    assert_eq!(findings[0].effort, Effort::High);
    assert!(findings[0].code_provable);
}

#[test]
fn does_not_raise_a_lower_confidence() {
    // The cap is a ceiling, never an assignment — a claim the model itself rated
    // low must not be *promoted* to the advisory band.
    let mut findings = vec![ruff_finding()];
    findings[0].confidence = 0.21;
    demote_ungrounded_registry_claims(&mut findings);
    assert!(
        (findings[0].confidence - 0.21).abs() < f32::EPSILON,
        "got {}",
        findings[0].confidence
    );
}

#[test]
fn does_not_promote_a_low_effort_claim() {
    let mut findings = vec![ruff_finding()];
    findings[0].effort = Effort::Low;
    demote_ungrounded_registry_claims(&mut findings);
    assert_eq!(findings[0].effort, Effort::Low, "Low must stay Low");
}

#[test]
fn pass_is_idempotent() {
    let mut findings = vec![ruff_finding()];
    assert_eq!(demote_ungrounded_registry_claims(&mut findings), 1);
    let after_first = findings[0].description.clone();

    assert_eq!(
        demote_ungrounded_registry_claims(&mut findings),
        0,
        "a second run must be a no-op"
    );
    assert_eq!(
        findings[0].description, after_first,
        "the advisory note must not be appended twice"
    );
}

// ─── Constant / note invariants ──────────────────────────────────────────────

/// The advisory-band invariants, as compile-time assertions.
///
/// Written as `const _: () = assert!(..)` (the idiom `config::constants`'s own
/// `threshold_ordering` test uses) so a bad edit to
/// [`UNGROUNDED_CLAIM_ADVISORY_CONFIDENCE`] fails the BUILD rather than one test
/// run — the value is load-bearing against thresholds in two other modules.
#[test]
fn advisory_confidence_sits_in_the_visible_but_non_escalating_band() {
    // At or above the review-include gate, or the finding vanishes — that would
    // trade #4081's false BLOCK for a false silence.
    const _: () = assert!(UNGROUNDED_CLAIM_ADVISORY_CONFIDENCE >= FIX_ISSUE_MIN_CONFIDENCE);
    // Below the tracker-issue gate: an unchecked claim must not open an issue.
    const _: () = assert!(UNGROUNDED_CLAIM_ADVISORY_CONFIDENCE < BLOCK_ISSUE_MIN_CONFIDENCE);
    // At or below `grade.rs`'s advisory-batch collapse line — that override is
    // what dissolves a model self-reported BLOCK. The constant is private there,
    // so assert against its documented value (0.65); a change there trips this
    // rather than silently un-fixing #4081.
    const _: () = assert!(UNGROUNDED_CLAIM_ADVISORY_CONFIDENCE <= 0.65);
}

#[test]
fn advisory_note_does_not_trip_finding_hygiene_markers() {
    // The note is appended to `description`; if it contained a hygiene marker,
    // a later pass would drop or re-demote the very finding it annotates.
    let mut findings = vec![ruff_finding()];
    demote_ungrounded_registry_claims(&mut findings);
    let mut annotated: Vec<Finding> = findings.clone();

    let counts = crate::pipeline::finding_hygiene::sanitize_findings(&mut annotated);
    assert_eq!(counts.dropped_self_negated, 0, "note must not self-negate");
    assert_eq!(
        counts.demoted_diff_absent, 0,
        "note must not read as speculation"
    );
    assert_eq!(
        counts.demoted_ungrounded_registry, 0,
        "the sentinel must stop the note re-triggering its own pass"
    );
    assert_eq!(annotated.len(), 1);
}
