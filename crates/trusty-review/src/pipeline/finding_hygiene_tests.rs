//! Unit tests for `finding_hygiene` (#4043, #4044), using verbatim (or
//! near-verbatim, where the issue only quoted a fragment) text from the two
//! issues so the fixtures are traceable back to the reported evidence.

use super::*;
use crate::models::Finding;

fn finding(effort: Effort, description: &str) -> Finding {
    Finding::new(
        "src/app.ts",
        "logic-error",
        description,
        "fix it",
        0.8,
        effort,
    )
}

// ─── #4044 defect 1: self-withdrawn findings ──────────────────────────────────

#[test]
fn drops_finding_containing_each_self_negation_marker() {
    // Verbatim closing lines from #4044's table (findings 22, 29, 30, 31, 32,
    // 33, 34) plus review 2's finding 27.
    let verbatim_closers = [
        "This is correctly implemented.",
        "This appears correct — noting it for completeness.",
        "The code is correct.",
        "No actual bug here — flagging as low confidence.",
        "Withdrawing this finding — confidence too low.",
        "This is a praise-worthy design decision, not a finding. Withdrawing.",
        "This flow is actually correct. No finding here — the setAssembled(null) \
         in toggleAffirmation handles it properly. Withdrawing this finding.",
        "On re-inspection this is actually correct — both are cleared at the start. \
         This is a non-issue.",
    ];
    for closer in verbatim_closers {
        let mut findings = vec![finding(Effort::Medium, closer)];
        let dropped = drop_self_negated_or_leaked_findings(&mut findings);
        assert_eq!(dropped, 1, "must drop self-negated finding: {closer:?}");
        assert!(findings.is_empty());
    }
}

#[test]
fn does_not_drop_legitimate_finding_using_the_word_correct() {
    // A real finding may legitimately use "correct" in a non-self-negating way
    // (e.g. contrasting correct vs. broken behavior) — must survive.
    let mut findings = vec![finding(
        Effort::High,
        "The OLD validation path was correct, but this diff removes the null \
         check that made it so, introducing a panic on empty input.",
    )];
    let dropped = drop_self_negated_or_leaked_findings(&mut findings);
    assert_eq!(dropped, 0, "a legitimate finding must not be false-dropped");
    assert_eq!(findings.len(), 1);
}

// ─── #4044 defect 2: chain-of-thought leak ────────────────────────────────────

#[test]
fn drops_finding_with_leaked_chain_of_thought() {
    // Verbatim (abbreviated) from #4044's finding 32 — the full published body
    // that begins with a fabricated ordering, then narrates a live correction.
    let leaked = "In POST /api/admin/rounds/:roundId/voters, the JSON parse and Zod \
        validation happen AFTER the MUTABLE_COHORT_STATUSES check. Wait — actually \
        looking at the code more carefully, the order is different. So a request \
        with a malformed body against a closed round returns 400. Withdrawing this \
        finding — confidence too low.";
    let mut findings = vec![finding(Effort::Medium, leaked)];
    let dropped = drop_self_negated_or_leaked_findings(&mut findings);
    assert_eq!(
        dropped, 1,
        "a finding leaking raw deliberation must be dropped"
    );
    assert!(findings.is_empty());
}

#[test]
fn drops_finding_with_deliberation_marker_even_without_withdrawal() {
    // The deliberation-leak markers alone (no explicit "withdrawing") must
    // still trip the filter — CoT must never reach output regardless of
    // whether the finding ultimately re-affirms itself.
    let mut findings = vec![finding(
        Effort::High,
        "Wait — actually looking at the code more carefully, this does look like \
         a real null-deref on line 42.",
    )];
    let dropped = drop_self_negated_or_leaked_findings(&mut findings);
    assert_eq!(
        dropped, 1,
        "raw deliberation text must never reach user-visible output"
    );
}

// ─── #4044: verdict excludes withdrawn findings (integration of the two fixes) ─

#[test]
fn sanitize_findings_removes_withdrawn_before_verdict_would_see_them() {
    use crate::models::Verdict;
    use crate::pipeline::grade::derive_verdict;

    // 34-finding-shaped batch: 1 real BLOCK-driving finding + 3 self-withdrawn.
    let mut findings = vec![
        finding(
            Effort::High,
            "SQL injection: raw user input concatenated into query.",
        ),
        finding(
            Effort::Medium,
            "Withdrawing this finding — confidence too low.",
        ),
        finding(Effort::Medium, "The code is correct."),
        finding(Effort::Low, "This is a non-issue."),
    ];
    // Give the real finding a citation so it clears the escalation gate.
    findings[0].code_provable = true;

    sanitize_findings(&mut findings);
    assert_eq!(
        findings.len(),
        1,
        "only the real finding must survive to reach the verdict floor"
    );
    let verdict = derive_verdict(Verdict::Block, &findings);
    assert_eq!(
        verdict,
        Verdict::Block,
        "the real finding still drives BLOCK on its own merits"
    );
}

#[test]
fn sanitize_findings_floor_no_longer_counts_withdrawn_findings() {
    use crate::models::Verdict;
    use crate::pipeline::grade::derive_verdict;

    // The literal #4044 crux: "findings that the reviewer itself has withdrawn
    // ... are entering the aggregation at all." A self-withdrawn High finding
    // must no longer be able to drive the deterministic BLOCK floor once
    // `sanitize_findings` runs, even though the model's own top-line verdict
    // (represented here by the `derive_verdict` seed) did not itself change.
    let mut findings = vec![
        finding(
            Effort::High,
            "Withdrawing this finding — confidence too low.",
        ),
        finding(Effort::Low, "Minor style nit."),
    ];
    findings[0].code_provable = true; // escalation-eligible — would drive BLOCK

    let before = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(
        before,
        Verdict::Block,
        "precondition: the withdrawn-but-not-yet-sanitized High finding still \
         forces the BLOCK floor"
    );

    sanitize_findings(&mut findings);
    assert_eq!(findings.len(), 1, "only the real Low finding survives");

    let after = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(
        after,
        Verdict::Approve,
        "a withdrawn finding must never drive the verdict floor"
    );
}

// ─── #4043: spec reported as implementation ───────────────────────────────────

#[test]
fn demotes_finding_admitting_diff_absent_speculation() {
    // Near-verbatim from #4043 review 2, finding 6.
    let mut findings = vec![finding(
        Effort::High,
        "However, the diff contains only the spec document \
         (docs/specs/admin-judging-api.md) and migrations — no implementation of \
         src/lib/judging/aggregate.ts, src/lib/judging/z-score.ts, or any other \
         scoring engine file is present. If implementation follows the interface \
         literally, the COI filter for stack-rank picks will be absent.",
    )];
    findings[0].code_provable = true;

    let demoted = demote_diff_absent_speculation(&mut findings);
    assert_eq!(demoted, 1);
    assert_eq!(findings[0].effort, Effort::Medium);
    assert!(
        !findings[0].code_provable,
        "a claim about an absent implementation cannot be diff-provable"
    );
    assert!(
        !crate::pipeline::grade::drives_block_floor(&findings[0]),
        "demoted finding must never drive the BLOCK floor"
    );
}

#[test]
fn demotes_finding_admitting_spec_document_not_implemented() {
    // Near-verbatim from #4043 review 1, finding 7.
    let mut findings = vec![finding(
        Effort::High,
        "Since this is a spec document (not yet implemented code), the consequence \
         is that an implementer following §5 literally would introduce the \
         denial-of-service bug the spec's §2 section explicitly analyzed and \
         rejected.",
    )];
    let demoted = demote_diff_absent_speculation(&mut findings);
    assert_eq!(demoted, 1);
    assert_eq!(findings[0].effort, Effort::Medium);
}

#[test]
fn does_not_touch_medium_diff_absent_finding() {
    // Already below High — cannot drive BLOCK regardless; must be left alone
    // (no spurious mutation of a finding the floor already treats safely).
    let mut findings = vec![finding(
        Effort::Medium,
        "If implementation follows the interface literally, a field would be missing.",
    )];
    let demoted = demote_diff_absent_speculation(&mut findings);
    assert_eq!(demoted, 0);
    assert_eq!(findings[0].effort, Effort::Medium);
}

#[test]
fn does_not_demote_legitimate_implementation_grounded_finding() {
    // A real, diff-grounded High finding with no diff-absent admission must
    // survive completely untouched.
    let mut findings = vec![finding(
        Effort::High,
        "The diff removes the null check before dereferencing `user.profile`, \
         a real null-deref provable from the code under review.",
    )];
    findings[0].code_provable = true;
    let demoted = demote_diff_absent_speculation(&mut findings);
    assert_eq!(demoted, 0);
    assert_eq!(findings[0].effort, Effort::High);
    assert!(findings[0].code_provable);
}

// ─── Orchestration ─────────────────────────────────────────────────────────────

#[test]
fn sanitize_findings_runs_every_pass() {
    let mut findings = vec![
        finding(
            Effort::Medium,
            "Withdrawing this finding — confidence too low.",
        ),
        finding(
            Effort::High,
            "If implementation follows the interface literally, this breaks.",
        ),
        finding(
            Effort::High,
            "The workflow runs `pip install \"ruff==0.16.0\"`, but version 0.16.0 \
             does not exist on PyPI.",
        ),
        finding(Effort::High, "Real SQL injection provable from the diff."),
    ];
    let counts = sanitize_findings(&mut findings);
    assert_eq!(counts.dropped_self_negated, 1);
    assert_eq!(counts.demoted_diff_absent, 1);
    assert_eq!(counts.demoted_ungrounded_registry, 1, "#4081 pass is wired");
    assert_eq!(findings.len(), 3);
    assert_eq!(findings[0].effort, Effort::Medium, "demoted, not dropped");
    assert_eq!(
        findings[1].effort,
        Effort::Medium,
        "#4081 demoted, not dropped"
    );
    assert_eq!(findings[2].effort, Effort::High, "untouched real finding");
}

// ─── relax_verdict_if_evidence_wiped (#4042, #4044) ───────────────────────────

#[test]
fn relaxes_verdict_when_all_findings_wiped_this_run() {
    // The full end-to-end guarantee: a model that self-reported BLOCK on the
    // strength of findings we then wiped out entirely (fabricated + withdrawn)
    // must have its OWN raw verdict/grade relaxed too — not just the floor.
    let mut verdict = Verdict::Block;
    let mut grade = Some("F".to_string());
    relax_verdict_if_evidence_wiped(&mut verdict, &mut grade, 3, &[]);
    assert_eq!(verdict, Verdict::Approve);
    assert_eq!(grade, None);
}

#[test]
fn does_not_relax_when_findings_were_already_empty() {
    // `findings_before == 0` means there was never any evidence to begin with
    // (a clean APPROVE with no findings, or a genuinely finding-free BLOCK from
    // some other signal) — this is NOT the "we just wiped the evidence" case
    // and must be left alone.
    let mut verdict = Verdict::Block;
    let mut grade = Some("F".to_string());
    relax_verdict_if_evidence_wiped(&mut verdict, &mut grade, 0, &[]);
    assert_eq!(verdict, Verdict::Block, "not our call to relax this one");
}

#[test]
fn does_not_relax_when_findings_survive() {
    let survivor = finding(Effort::High, "Real SQL injection provable from the diff.");
    let mut verdict = Verdict::Block;
    let mut grade = Some("F".to_string());
    relax_verdict_if_evidence_wiped(&mut verdict, &mut grade, 2, std::slice::from_ref(&survivor));
    assert_eq!(
        verdict,
        Verdict::Block,
        "real surviving evidence still backs BLOCK"
    );
}

#[test]
fn does_not_touch_an_already_approve_verdict() {
    let mut verdict = Verdict::Approve;
    let mut grade = Some("A-".to_string());
    relax_verdict_if_evidence_wiped(&mut verdict, &mut grade, 2, &[]);
    assert_eq!(verdict, Verdict::Approve);
    assert_eq!(grade.as_deref(), Some("A-"), "no-op — nothing to relax");
}
