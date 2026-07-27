//! Regression tests for #4042: fabricated findings from diff-path
//! misattribution returned on trusty-review 0.10.1 (regression of #2881 /
//! #2882).
//!
//! The fixture reproduces the issue's "centerpiece" shape as closely as
//! possible without access to the reporter's private repo (`duettoresearch/
//! code-intelligence`, not reachable from this environment): a real TS module
//! (`src/lib/team-registration-early-access.ts`) diffed ALONGSIDE a Drizzle
//! JSON snapshot and a Vitest test file — the exact "adjacent `.json` /
//! `.test.ts`" shape #4042 itself suggests as the reproducing case — plus the
//! VERBATIM finding text quoted in the issue (findings 1, 2, 3, and 33).
//!
//! Two things are locked in:
//!  (a) each of the fabricated findings (verbatim from #4042) is dropped —
//!      never downgraded-but-kept, never emitted;
//!  (b) a LEGITIMATE finding that correctly quotes the real file survives
//!      unchanged, proving the fix does not suppress real findings alongside
//!      invented ones.

use trusty_review::models::{Effort, Finding, Verdict};
use trusty_review::pipeline::citation_check::{DiffContentIndex, enforce_citation_integrity};
use trusty_review::pipeline::derive_verdict;
use trusty_review::pipeline::diff_analyzer::DiffAnalyzer;

/// The real content of `src/lib/team-registration-early-access.ts` (ground
/// truth per #4042: "an ordinary TypeScript module that exports exactly the
/// function finding #4 says is missing: `isTeamRegistrationOpenFor`").
const REAL_MODULE: &str =
    "export function isTeamRegistrationOpenFor(round) {\n  return round.status === 'open';\n}\n";

/// Build a multi-file diff: the real module, adjacent to a Drizzle JSON
/// snapshot and a Vitest test file — the shape #4042 itself suggests as the
/// reproducing case for diff-hunk-to-path misattribution.
fn repro_diff() -> String {
    let mut d = String::new();

    d.push_str("diff --git a/src/lib/team-registration-early-access.ts b/src/lib/team-registration-early-access.ts\n");
    d.push_str("new file mode 100644\nindex 0000000..1111111\n");
    d.push_str("--- /dev/null\n+++ b/src/lib/team-registration-early-access.ts\n");
    d.push_str("@@ -0,0 +1,3 @@\n");
    for line in REAL_MODULE.lines() {
        d.push('+');
        d.push_str(line);
        d.push('\n');
    }

    d.push_str("diff --git a/drizzle/meta/0007_snapshot.json b/drizzle/meta/0007_snapshot.json\n");
    d.push_str("new file mode 100644\nindex 0000000..2222222\n");
    d.push_str("--- /dev/null\n+++ b/drizzle/meta/0007_snapshot.json\n");
    d.push_str("@@ -0,0 +1,1 @@\n");
    d.push_str("+{ \"id\": \"893644d4-aaaa-bbbb-cccc-000000000000\", \"tables\": {} }\n");

    d.push_str("diff --git a/src/lib/draft-form.test.ts b/src/lib/draft-form.test.ts\n");
    d.push_str("new file mode 100644\nindex 0000000..3333333\n");
    d.push_str("--- /dev/null\n+++ b/src/lib/draft-form.test.ts\n");
    d.push_str("@@ -0,0 +1,2 @@\n");
    d.push_str("+import { describe, expect, it } from 'vitest';\n");
    d.push_str("+describe('draft-form', () => {});\n");

    d
}

/// Finding 1 (verbatim from #4042): claims the real module is a Drizzle JSON
/// snapshot, quoting `drizzle/meta/0007_snapshot.json`'s content as if it were
/// `team-registration-early-access.ts`'s own content.
fn fabricated_finding_1() -> Finding {
    let mut f = Finding::new(
        "src/lib/team-registration-early-access.ts",
        "logic-error",
        "The first file in the diff is team-registration-early-access.ts but \
         its entire content is a Drizzle ORM JSON snapshot (a drizzle/meta/ artifact). \
         [code: `src/lib/team-registration-early-access.ts:1` — \"id: 893644d4 fabricated snapshot content\"].",
        "The module never provides isTeamRegistrationOpenFor, breaking every caller.",
        0.8,
        Effort::High,
    );
    f.line = Some(1);
    f.code_provable = true;
    f
}

/// Finding 2 (verbatim from #4042): claims the real module is a Vitest suite
/// for `draft-form.ts`, quoting the ADJACENT test file's import line.
fn fabricated_finding_2() -> Finding {
    let mut f = Finding::new(
        "src/lib/team-registration-early-access.ts",
        "logic-error",
        "The file team-registration-early-access.ts is named and pathed as if \
         it were a library module, but its content is entirely a Vitest test suite \
         (describe/it/expect) for draft-form.ts. \
         [code: `src/lib/team-registration-early-access.ts:1` — \"import { describe, expect, it } from 'vitest'\"].",
        "No isTeamRegistrationOpenFor export exists; the build fails.",
        0.8,
        Effort::High,
    );
    f.line = Some(1);
    f.code_provable = true;
    f
}

/// Finding 33 (verbatim shape from #4042): a SHORT, sub-`MIN_SPAN_LEN` quote
/// ("publishedAt", 11 chars) that individual content-verification alone
/// fails-open on — this is the finding the mutual-contradiction backstop is
/// for, since it shares `src/lib/team-registration-early-access.ts:1` with
/// findings 1 and 2 above.
fn fabricated_finding_33() -> Finding {
    let mut f = Finding::new(
        "src/lib/team-registration-early-access.ts",
        "logic-error",
        "In `team-registration-early-access.ts` (the override route), the new row \
         inherits `publishedAt` from the superseded row. \
         [code: `src/lib/team-registration-early-access.ts:1` — \"override route handler entrypoint\"].",
        "Stale publishedAt values leak into new rows.",
        0.7,
        Effort::Medium,
    );
    f.line = Some(1);
    f
}

/// A LEGITIMATE finding, correctly grounded in the real module's own content —
/// must survive the fix unchanged.
fn legitimate_finding() -> Finding {
    let mut f = Finding::new(
        "src/lib/team-registration-early-access.ts",
        "logic-error",
        "isTeamRegistrationOpenFor compares `round.status === 'open'` with no null guard \
         [code: `src/lib/team-registration-early-access.ts:2` — \"round.status === 'open'\"].",
        "Throws if round is null.",
        0.85,
        Effort::High,
    );
    f.line = Some(2);
    f.code_provable = true;
    f
}

/// (a)+(b): after citation-integrity enforcement, all three fabricated
/// findings are dropped and the legitimate finding survives untouched.
#[tokio::test]
async fn fabricated_centerpiece_findings_are_dropped_legit_finding_survives() {
    let diff = repro_diff();
    let filtered = DiffAnalyzer::default().analyze(&diff).await;
    let index = DiffContentIndex::from_filtered(&filtered);

    let mut findings = vec![
        fabricated_finding_1(),
        fabricated_finding_2(),
        fabricated_finding_33(),
        legitimate_finding(),
    ];
    let before_verdict = derive_verdict(Verdict::Block, &findings);
    assert_eq!(
        before_verdict,
        Verdict::Block,
        "precondition: the fabricated Highs force BLOCK before enforcement"
    );

    let dropped = enforce_citation_integrity(&mut findings, &index);
    assert_eq!(
        dropped, 3,
        "all three fabricated findings must be dropped: {findings:#?}"
    );
    assert_eq!(findings.len(), 1, "only the legitimate finding survives");
    assert_eq!(
        findings[0].description,
        legitimate_finding().description,
        "the surviving finding must be the legitimate one, verbatim"
    );

    // The verdict now reflects only the legitimate (still-High, still-grounded)
    // finding — BLOCK survives because it is a REAL grounded citation, not
    // because fabrications forced it.
    let after_verdict = derive_verdict(Verdict::Approve, &findings);
    assert_eq!(
        after_verdict,
        Verdict::Block,
        "the legitimate high-severity finding must still drive its own floor"
    );
}

/// End-to-end: when EVERY finding in a review is fabricated (no legitimate
/// finding at all — the worst case), the model's own self-reported BLOCK must
/// ALSO relax, not just the deterministic floor (#4042, #4044). This is the
/// literal "fabricated findings block clean PRs" symptom #4042 reports.
#[tokio::test]
async fn fully_fabricated_review_relaxes_the_top_level_verdict_too() {
    use trusty_review::pipeline::finding_hygiene::relax_verdict_if_evidence_wiped;

    let diff = repro_diff();
    let filtered = DiffAnalyzer::default().analyze(&diff).await;
    let index = DiffContentIndex::from_filtered(&filtered);

    let mut findings = vec![
        fabricated_finding_1(),
        fabricated_finding_2(),
        fabricated_finding_33(),
    ];
    let findings_before = findings.len();

    // The model itself (having believed its own fabrications) self-reported
    // BLOCK/F — this is the raw top-level field, not derived from the floor.
    let mut model_verdict = Verdict::Block;
    let mut model_grade = Some("F".to_string());

    let dropped = enforce_citation_integrity(&mut findings, &index);
    assert_eq!(dropped, 3, "every fabricated finding must be dropped");
    assert!(findings.is_empty());

    relax_verdict_if_evidence_wiped(
        &mut model_verdict,
        &mut model_grade,
        findings_before,
        &findings,
    );
    assert_eq!(
        model_verdict,
        Verdict::Approve,
        "the model's own raw BLOCK must relax once every supporting finding is gone"
    );
    assert_eq!(model_grade, None);

    // And the floor agrees.
    let final_verdict = derive_verdict(model_verdict, &findings);
    assert_eq!(final_verdict, Verdict::Approve);
}

/// The independent #4042 incident: a finding citing a path/line that was
/// NEVER part of the diff at all (`hotelPage.ts:207`, no relationship to this
/// diff) must be dropped as unresolvable, not fail-open.
#[tokio::test]
async fn citation_to_a_path_outside_the_diff_is_dropped() {
    let diff = repro_diff();
    let filtered = DiffAnalyzer::default().analyze(&diff).await;
    let index = DiffContentIndex::from_filtered(&filtered);

    let mut f = Finding::new(
        "hotelPage.ts",
        "logic-error",
        "Promise.race never resolves because the loser promise is never cancelled.",
        "hangs forever",
        0.7,
        Effort::High,
    );
    f.line = Some(207);
    f.code_provable = true;
    let mut findings = vec![f];

    let dropped = enforce_citation_integrity(&mut findings, &index);
    assert_eq!(dropped, 1, "an out-of-diff citation must be dropped");
    assert!(findings.is_empty());
}
