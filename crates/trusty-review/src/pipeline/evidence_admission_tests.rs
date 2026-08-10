//! Unit tests for `evidence_admission` (#5309).
//!
//! Fixtures use the VERBATIM finding #5309 reports (the un-awaited `run` calls
//! against `bobmatnyc/trusty-tools` PR 5303) so they stay traceable to the
//! reported evidence, plus the negative cases that keep the guard from
//! over-firing.

use super::*;
use crate::config::constants::FIX_ISSUE_MIN_CONFIDENCE;
use crate::models::{Finding, Verdict};
use crate::pipeline::grade::drives_block_floor;

/// The finding #5309 reports, field for field.
fn unawaited_run_finding() -> Finding {
    let mut f = Finding::new(
        "crates/trusty-git-analytics/src/audit/sweep.rs",
        "async functions called without .await",
        "`incidents::run`, `dora::run`, `pr_metrics::run` and `report::run` are \
         invoked without `.await`, so half the sweep pipeline would construct a \
         future and drop it, silently no-opping. The diff does not show their \
         signatures, so this cannot be confirmed from the diff alone.",
        "Add `.await` to each of the four calls.",
        0.72,
        Effort::High,
    );
    f.line = Some(64);
    f.consequence = "Half the audit pipeline silently produces no output".to_string();
    f.code_provable = true;
    f
}

// ─── The reported defect ─────────────────────────────────────────────────────

#[test]
fn demotes_the_verbatim_5309_finding() {
    let mut findings = vec![unawaited_run_finding()];
    assert_eq!(demote_self_admitted_unverifiable(&mut findings), 1);

    let f = &findings[0];
    assert!(
        !f.code_provable,
        "a claim the finding says it could not check is not provable from the diff"
    );
    assert_eq!(
        f.effort,
        Effort::Medium,
        "capped out of the BLOCK-floor tier"
    );
    assert!(
        (f.confidence - UNVERIFIABLE_ADVISORY_CONFIDENCE).abs() < f32::EPSILON,
        "confidence capped at the advisory band, got {}",
        f.confidence
    );
    assert!(
        matches!(f.verified, Some(VerifyOutcome::Unverifiable { .. })),
        "must be pre-stamped Unverifiable so the verifier is never asked, got {:?}",
        f.verified
    );
}

#[test]
fn demoted_finding_does_not_drive_the_block_floor() {
    let mut findings = vec![unawaited_run_finding()];
    assert!(
        drives_block_floor(&findings[0]),
        "precondition: the reported finding WOULD pin the BLOCK floor before the fix"
    );
    demote_self_admitted_unverifiable(&mut findings);
    assert!(
        !drives_block_floor(&findings[0]),
        "a self-admittedly unchecked claim must not drive BLOCK"
    );
}

#[test]
fn demoted_finding_is_never_a_verification_candidate() {
    let mut findings = vec![unawaited_run_finding()];
    assert_eq!(
        crate::pipeline::verify::select_candidates(Verdict::Block, &findings),
        vec![0],
        "precondition: it was a candidate before the demotion"
    );
    demote_self_admitted_unverifiable(&mut findings);
    assert!(
        crate::pipeline::verify::select_candidates(Verdict::Block, &findings).is_empty(),
        "a pre-stamped Unverifiable finding must never reach the verifier"
    );
}

#[test]
fn demoted_finding_still_surfaces_to_the_author() {
    let mut findings = vec![unawaited_run_finding()];
    demote_self_admitted_unverifiable(&mut findings);
    let f = &findings[0];
    assert_eq!(f.file, "crates/trusty-git-analytics/src/audit/sweep.rs");
    assert_eq!(f.line, Some(64));
    assert!(
        f.description.contains("`incidents::run`"),
        "the original claim text must survive verbatim"
    );
    assert!(
        f.description.contains("Unchecked claim (#5309)"),
        "the reader must be told nothing checked it"
    );
    assert!(
        f.confidence >= FIX_ISSUE_MIN_CONFIDENCE,
        "stays above the review-include gate so it is not silently suppressed"
    );
}

// ─── Marker coverage ─────────────────────────────────────────────────────────

#[test]
fn demotes_each_admission_marker() {
    for marker in SELF_ADMITTED_UNVERIFIABLE_MARKERS {
        let mut findings = vec![Finding::new(
            "src/a.rs",
            "logic-error",
            format!("The call is wrong; {marker} caller."),
            "fix it",
            0.9,
            Effort::High,
        )];
        assert_eq!(
            demote_self_admitted_unverifiable(&mut findings),
            1,
            "marker {marker:?} must demote"
        );
    }
}

#[test]
fn does_not_demote_finding_describing_unverified_code() {
    // "unverified" / "cannot be confirmed" about the CODE's behaviour, not about
    // the finding's own evidence — must keep full weight.
    let mut findings = vec![Finding::new(
        "src/auth.rs",
        "security",
        "The JWT payload is used unverified: `decode_unchecked` skips signature \
         validation, so a forged token cannot be confirmed as authentic and is \
         accepted anyway.",
        "Use `decode` with the verifying key.",
        0.95,
        Effort::High,
    )];
    assert_eq!(
        demote_self_admitted_unverifiable(&mut findings),
        0,
        "a real finding about unverified code must not be demoted"
    );
    assert!(findings[0].verified.is_none());
}

#[test]
fn does_not_touch_a_finding_with_a_decided_outcome() {
    let mut findings = vec![unawaited_run_finding()];
    findings[0].verified = Some(VerifyOutcome::Confirmed);
    assert_eq!(demote_self_admitted_unverifiable(&mut findings), 0);
    assert!(matches!(
        findings[0].verified,
        Some(VerifyOutcome::Confirmed)
    ));
}

#[test]
fn pass_is_idempotent() {
    let mut findings = vec![unawaited_run_finding()];
    assert_eq!(demote_self_admitted_unverifiable(&mut findings), 1);
    let after_first = findings[0].description.clone();
    assert_eq!(demote_self_admitted_unverifiable(&mut findings), 0);
    assert_eq!(findings[0].description, after_first, "note appended twice");
}

#[test]
fn advisory_note_does_not_trip_sibling_pass_markers() {
    // The note must not cause a later hygiene pass to drop or re-demote the
    // finding it annotates.
    let mut annotated = vec![Finding::new(
        "src/a.rs",
        "logic-error",
        format!("A perfectly ordinary finding.\n\n{SELF_ADMITTED_NOTE}"),
        "fix it",
        0.9,
        Effort::High,
    )];
    let counts = crate::pipeline::finding_hygiene::sanitize_findings(&mut annotated);
    assert_eq!(annotated.len(), 1, "the note must not cause a drop");
    assert_eq!(counts.dropped_self_negated, 0);
    assert_eq!(counts.demoted_diff_absent, 0);
    assert_eq!(counts.demoted_ungrounded_registry, 0);
    assert_eq!(counts.demoted_self_admitted_unverifiable, 0);
}

// ─── Shared demotion primitive ───────────────────────────────────────────────

#[test]
fn demote_is_a_ceiling_never_a_promotion() {
    let mut f = Finding::new("src/a.rs", "nit", "d", "s", 0.2, Effort::Low);
    demote_to_unverifiable_advisory(&mut f);
    assert!(
        (f.confidence - 0.2).abs() < f32::EPSILON,
        "a lower confidence must be left alone, got {}",
        f.confidence
    );
    assert_eq!(f.effort, Effort::Low, "a lower effort must be left alone");
}
