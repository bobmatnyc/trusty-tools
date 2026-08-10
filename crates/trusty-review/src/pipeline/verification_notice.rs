//! Post-verification correction banner for the narrative summary (#4044).
//!
//! Why: the prose summary and the verdict are produced at different times from
//! different evidence, and only the verdict is revisited.
//!
//! On the unified path `review_body` is the reviewer LLM's own text, set by
//! `apply_llm_response` in `runner.rs` step 6; on the map-reduce path it is the
//! synthesis pass's summary, set in `runner_mapreduce.rs`. Both are written
//! BEFORE `maybe_verify` runs. Verification then records `Refuted` on individual
//! findings and re-derives the verdict from the survivors
//! (`verify::rederive_verdict` excludes them, `grade::is_substantive` excludes
//! them again) — but nothing goes back and revisits the prose. The summary keeps
//! naming findings the verifier subsequently disproved.
//!
//! On PR #5308, `review_pr` returned six findings, three carrying `"verified":
//! "refuted"` and `"confidence": 0.1`, and the summary still read:
//!
//! > "finding #1 is a high-effort defect ... both must be resolved before merge."
//!
//! Finding index 0 was one of the refuted three. The refutation was correct; the
//! summary was wrong.
//!
//! ## What: correct the summary, do not rewrite it
//!
//! Re-synthesising the prose would mean a second LLM call after verification, on
//! every review, to restate what the pipeline already knows deterministically.
//! [`prepend_verification_notice`] instead prepends a banner that names each
//! refuted finding by the same 1-based index the prose uses, so a reader who
//! reaches "finding #1 ... must be resolved before merge" has already been told
//! that finding #1 did not survive verification. It is the same shape as the
//! `#590` degraded banner and the map-reduce coverage notice: a deterministic
//! qualifier on a narrative the pipeline cannot regenerate.
//!
//! ## Scope: clean refutations only
//!
//! Only `VerifyOutcome::Refuted` — the verifier examined the finding and
//! disproved it — is named. `ErrorRefuted` and `TruncationRefuted` mean "we could
//! not reach the verifier" (#726, #1876), and `rederive_verdict` path (c)
//! deliberately PRESERVES the escalation for those. Calling them non-blockers
//! would contradict the verdict the same review reports.
//!
//! ## What it deliberately does NOT do
//!
//! It does not drop refuted findings from `findings`. Spec REV-606 requires the
//! outcome to stay on the result for transparency, `verify::apply_outcome`
//! documents the same, and `findings_count` is defined (#1877) as mirroring
//! `findings.len()` — so the array keeps them, `findings_count` keeps counting
//! them, and the banner carries the refuted count separately.
//!
//! Test: `verification_notice_tests.rs` (unit) and
//! `run_review_refuted_finding_does_not_drive_grade_or_summary`
//! (runner_tests.rs — end-to-end through `run_review`).

use crate::models::{Finding, VerifyOutcome};

/// Sentinel opening the banner, used to keep the prepend idempotent.
///
/// Why: `finalize_review` is the single canonical exit for both pipeline paths,
/// but a caller that finalises a result twice (or a test that does) must not
/// stack two banners.
/// What: the literal prefix of the rendered banner.
/// Test: `prepend_is_idempotent`.
const NOTICE_SENTINEL: &str = "> **Verification notice:**";

/// Prepend a banner naming every verifier-refuted finding, if any.
///
/// Why: see the module doc — the summary predates verification and cannot be
/// regenerated deterministically, so it is qualified rather than rewritten.
/// What: returns `body` unchanged when no finding carries
/// `VerifyOutcome::Refuted`, or when the banner is already present. Otherwise
/// returns the banner followed by a blank line and `body`. Findings are named by
/// their 1-based index into `findings` — the numbering the reviewer's own prose
/// uses ("finding #1" for index 0) — plus file and kind so a reader can match
/// them even when the prose numbers differently.
/// Test: `prepends_banner_naming_each_refuted_finding`,
/// `no_banner_when_nothing_was_refuted`,
/// `error_refuted_is_not_named_as_a_non_blocker`, `prepend_is_idempotent`.
pub fn prepend_verification_notice(body: &str, findings: &[Finding]) -> String {
    if body.contains(NOTICE_SENTINEL) {
        return body.to_string();
    }
    let refuted: Vec<(usize, &Finding)> = findings
        .iter()
        .enumerate()
        .filter(|(_, f)| matches!(f.verified, Some(VerifyOutcome::Refuted)))
        .collect();
    if refuted.is_empty() {
        return body.to_string();
    }

    let mut notice = format!(
        "{NOTICE_SENTINEL} the summary below was written before the per-finding \
         verification round. {n} of {total} finding(s) were REFUTED by the verifier \
         and do not support the verdict — they are not merge blockers, and any \
         reference to them below is superseded:\n",
        n = refuted.len(),
        total = findings.len(),
    );
    for (idx, f) in refuted {
        notice.push_str(&format!(
            "> - finding #{n} — `{file}`: {kind}\n",
            n = idx + 1,
            file = f.file,
            kind = f.kind,
        ));
    }
    notice.push('\n');
    notice.push_str(body);
    notice
}

#[cfg(test)]
#[path = "verification_notice_tests.rs"]
mod tests;
