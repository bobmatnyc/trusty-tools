//! End-to-end regression tests for #8905: a posted finding cites a line that
//! holds the code it describes, or it is not posted.
//!
//! Why: the evidence on #8905 is 4 of 7 fabricated findings citing a line 6-20
//! lines from the code they describe. These drive the whole `run_review` path
//! so they exercise the gate where the review tools and CLI reach it, and each
//! (a)–(d) case was red on `origin/main` before the gate landed, except (c),
//! which pins the #4999/#5023 behaviour the gate keeps.
//! Test: this module.

use super::*;

/// The line of `src/billing.rs` that holds `amounts.iter().sum::<u64>()`.
const SUM_LINE: u32 = 30;

/// A 40-line new file whose line [`SUM_LINE`] sums the amounts.
fn billing_diff() -> String {
    let mut diff = String::from(
        "diff --git a/src/billing.rs b/src/billing.rs\n--- a/src/billing.rs\n+++ b/src/billing.rs\n@@ -0,0 +1,40 @@\n",
    );
    for i in 1..=40 {
        if i == SUM_LINE {
            diff.push_str("+    let total = amounts.iter().sum::<u64>();\n");
        } else {
            diff.push_str(&format!("+    let value_{i} = step_{i}(input);\n"));
        }
    }
    diff
}

/// Review `billing_diff` with a reviewer that emits one Medium finding.
async fn review_one(body: &str, line: u32) -> ReviewResult {
    let (source, _tmp) = local_diff_source(&billing_diff());
    let finding = serde_json::json!({
        "title": "overflow",
        "body": body,
        "severity": "medium",
        "confidence": 0.9,
        "file": "src/billing.rs",
        "line": line,
    });
    let llm = FakeLlm {
        response: format!(
            "Overflow risk at src/billing.rs:{line}.\n\n```json\n{{\"verdict\":\"REQUEST_CHANGES\",\"summary\":\"overflow\",\"findings\":[{finding}]}}\n```"
        ),
        error: None,
        output_tokens: None,
    };
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
    };
    // #4044: only a verifier-confirmed finding is posted.
    let verifier: Arc<dyn LlmProvider> = Arc::new(FakeVerifier {
        judgment: "CONFIRMED",
    });
    run_review(
        &default_config(),
        input,
        ready_deps(Arc::new(llm), Some(verifier)),
    )
    .await
}

/// (a) A finding citing a line 12 off from its quoted code is re-anchored.
#[tokio::test]
async fn run_review_posts_the_reanchored_line() {
    let result = review_one(
        "`amounts.iter().sum::<u64>()` can overflow on large invoices.",
        SUM_LINE - 12,
    )
    .await;

    assert_eq!(result.findings.len(), 1, "{:?}", result.findings);
    assert_eq!(result.findings[0].line, Some(SUM_LINE));
    assert_eq!(
        result.findings[0]
            .citation_correction
            .map(|c| (c.from_line, c.to_line)),
        Some((Some(SUM_LINE - 12), SUM_LINE))
    );
}

/// (b) A finding whose quoted code is absent from the file is dropped. The
/// quote is under `citation_check`'s 12-character floor, so only the gate
/// catches it.
#[tokio::test]
async fn run_review_drops_a_finding_whose_quoted_code_is_absent() {
    let result = review_one("`flush_all()` is never awaited.", SUM_LINE).await;
    assert!(result.findings.is_empty(), "{:?}", result.findings);
    // #8905 row 4: a review the gate emptied is withheld, never APPROVE.
    // #9310: the reviewer asked for changes, so it is a suppressed rejection.
    assert_eq!(result.verdict, Verdict::RequestChanges);
    assert_eq!(
        result.verdict_status,
        Some(crate::models::VerdictStatus::SuppressedReject)
    );
    assert!(
        result
            .review_body
            .starts_with("1 findings withheld:\n- 1 citation unverifiable"),
        "{}",
        result.review_body
    );
}

/// (c) A finding citing a line beyond EOF is dropped (#4999/#5023 kept).
#[tokio::test]
async fn run_review_drops_a_finding_cited_beyond_eof() {
    let result = review_one("`amounts.iter().sum::<u64>()` can overflow.", 90).await;
    assert!(result.findings.is_empty(), "{:?}", result.findings);
}

/// (d) A finding with no anchor cannot be verified and is dropped.
#[tokio::test]
async fn run_review_drops_a_finding_with_no_anchor() {
    let result = review_one("This total can overflow on large invoices.", SUM_LINE).await;
    assert!(result.findings.is_empty(), "{:?}", result.findings);
    // #8905 row 4: a review the gate emptied is withheld, never APPROVE.
    // #9310: the reviewer asked for changes, so it is a suppressed rejection.
    assert_eq!(result.verdict, Verdict::RequestChanges);
    assert_eq!(
        result.verdict_status,
        Some(crate::models::VerdictStatus::SuppressedReject)
    );
    assert!(
        result
            .review_body
            .starts_with("1 findings withheld:\n- 1 citation unverifiable"),
        "{}",
        result.review_body
    );
}

/// Review `billing_diff` with a reviewer that writes `prose`, then a fenced
/// payload with `verdict`, `grade` and `findings`; the verifier answers
/// `judgment` for every finding (#9188).
async fn review_payload(
    prose: &str,
    verdict: &str,
    grade: &str,
    findings: serde_json::Value,
    judgment: &'static str,
) -> ReviewResult {
    review_diff_payload(&billing_diff(), prose, verdict, grade, findings, judgment).await
}

/// [`review_payload`] over any `diff`.
async fn review_diff_payload(
    diff: &str,
    prose: &str,
    verdict: &str,
    grade: &str,
    findings: serde_json::Value,
    judgment: &'static str,
) -> ReviewResult {
    let (source, _tmp) = local_diff_source(diff);
    let payload = serde_json::json!({
        "verdict": verdict,
        "grade": grade,
        "summary": prose,
        "findings": findings,
    });
    let llm = FakeLlm {
        response: format!("{prose}\n\n```json\n{payload}\n```"),
        error: None,
        output_tokens: None,
    };
    let input = ReviewInput {
        diff_source: source,
        reviewer_model: "openai/gpt-5.4-mini-20260317".to_string(),
        write_log: false,
        print_result: false,
        trigger: TriggerDecision::None,
        run_mode: RunMode::Cli,
        allow_posting: false,
        caller_context: CallerContext::default(),
        surface: InvocationSurface::default(),
    };
    let verifier: Arc<dyn LlmProvider> = Arc::new(FakeVerifier { judgment });
    run_review(
        &default_config(),
        input,
        ready_deps(Arc::new(llm), Some(verifier)),
    )
    .await
}

/// One reviewer finding against `src/billing.rs`.
fn billing_finding(title: &str, body: &str, severity: &str, line: u32) -> serde_json::Value {
    serde_json::json!({
        "title": title,
        "body": body,
        "severity": severity,
        "confidence": 0.9,
        "file": "src/billing.rs",
        "line": line,
    })
}

/// #9188 C: a defect the reviewer's prose names, whose finding was withheld,
/// never reaches the body by the prose route.
#[tokio::test]
async fn a_withheld_defect_named_in_the_summary_never_reaches_the_body() {
    let finding = billing_finding(
        "lost-write",
        "`ledger.flush_all()` is never awaited, so the invoice total is lost.",
        "high",
        SUM_LINE,
    );
    let result = review_payload(
        "The ledger flush is never awaited, so every invoice total is lost.",
        "REQUEST_CHANGES",
        "D",
        serde_json::json!([finding]),
        "CONFIRMED",
    )
    .await;
    assert!(result.findings.is_empty(), "{:?}", result.findings);
    assert!(
        !result.review_body.contains("never awaited"),
        "{}",
        result.review_body
    );
}

/// #9188 C: a clean review whose prose rests on its verified finding keeps it.
#[tokio::test]
async fn a_clean_review_keeps_its_prose() {
    let finding = billing_finding(
        "overflow",
        "`amounts.iter().sum::<u64>()` can overflow on large invoices.",
        "medium",
        SUM_LINE,
    );
    let result = review_payload(
        "Overflow risk at src/billing.rs:30 when invoices are large.",
        "REQUEST_CHANGES",
        "C",
        serde_json::json!([finding]),
        "CONFIRMED",
    )
    .await;
    assert_eq!(result.findings.len(), 1, "{:?}", result.withheld_findings);
    assert!(
        result
            .review_body
            .contains("Overflow risk at src/billing.rs:30"),
        "{}",
        result.review_body
    );
}

/// #9188 J (Architect ruling 2026-10-05 03:28Z): on a review that is not
/// `Unknown`, withheld findings never shape the grade. It stays present and is
/// recomputed from the survivors alone, so it differs from the grade the
/// withheld finding's severity would have produced.
#[tokio::test]
async fn run_review_withheld_findings_never_shape_the_grade() {
    let withheld = billing_finding(
        "lost-write",
        "`ledger.flush_all()` loses every invoice write.",
        "medium",
        SUM_LINE,
    );
    let nit = billing_finding(
        "naming",
        "`let value_2 = step_2(input);` needs a clearer name.",
        "low",
        2,
    );
    let result = review_payload(
        "One naming nit.",
        "APPROVE",
        "B-",
        serde_json::json!([withheld, nit]),
        "CONFIRMED",
    )
    .await;
    assert_eq!(result.findings.len(), 1, "{:?}", result.withheld_findings);
    assert_eq!(result.withheld_findings.len(), 1);
    assert_eq!(result.verdict, Verdict::Approve);

    let survivors_only = crate::pipeline::grade::derive_verdict(Verdict::Approve, &result.findings);
    let with_withheld = crate::pipeline::grade::derive_verdict(
        Verdict::Approve,
        &[
            result.findings[0].clone(),
            result.withheld_findings[0].finding.clone(),
        ],
    );
    assert_ne!(
        survivors_only, with_withheld,
        "the withheld severity changes the grade"
    );
    let would_have_been = crate::pipeline::letter_grade::reconcile_grade_with_verdict(
        crate::pipeline::letter_grade::default_grade_for_verdict(&with_withheld),
        &result.verdict,
    );
    assert_eq!(
        result.grade.as_deref(),
        Some("A+"),
        "recomputed from the survivor"
    );
    assert_ne!(result.grade, Some(would_have_been.to_string()));
}

/// What `run --json` reads off a review: its verdict, whether the run exits
/// non-zero, and the printed payload.
fn run_json(result: &ReviewResult) -> (Verdict, bool, serde_json::Value) {
    (
        result.verdict.clone(),
        crate::run_output::run_is_failure(result),
        crate::run_output::run_json_payload(result),
    )
}

/// AQ-7t (Bob 2026-10-05), end to end: an APPROVE review whose only finding
/// the citation gate withheld stays APPROVE and exits 0. The grade is
/// recomputed from the survivors (none) under the leak-J rule, so the model's
/// A- becomes A+; `run --json` carries `verdict_status` and the counts.
/// The quote is under `citation_check`'s 12-character floor, so the #8905
/// gate withholds it, not the pre-grade hygiene pass.
#[tokio::test]
async fn run_review_all_withheld_approve_stays_approve_and_exits_zero() {
    let nit = billing_finding("style", "`flush_all()` is slow.", "low", SUM_LINE);
    let result = review_payload(
        "Minor style nit only.",
        "APPROVE",
        "A-",
        serde_json::json!([nit]),
        "CONFIRMED",
    )
    .await;
    let (verdict, fails, json) = run_json(&result);
    assert_eq!(verdict, Verdict::Approve, "{:?}", result.error);
    assert!(
        !fails,
        "an all-withheld APPROVE exits 0: {:?}",
        result.error
    );
    assert_eq!(json["verdict"], "APPROVE");
    assert_eq!(json["verdict_status"], "all_withheld", "{json}"); // #9310
    assert_eq!(json["grade"], "A+", "graded from no survivor, not the A-");
    assert_eq!(json["withheld_count"], 1);
    assert_eq!(json["withheld_by_reason"]["line_citation"], 1, "{json}");
    assert!(json.get("error").is_none(), "{json}");
    assert!(result.findings.is_empty(), "{:?}", result.findings);
    assert_eq!(json["findings"], serde_json::json!([]));
    assert!(
        !result.review_body.contains("flush_all"),
        "{}",
        result.review_body
    );
}

/// AQ-7t, end to end, the 2026-10-01 code-intelligence shape: an APPROVE*
/// review whose only finding the verifier could not confirm stays APPROVE*,
/// exits 0, and is graded C+ (the APPROVE* band's best) from no survivor.
#[tokio::test]
async fn run_review_all_withheld_approve_star_stays_approve_star_and_exits_zero() {
    let note = billing_finding(
        "overflow-note",
        "`amounts.iter().sum::<u64>()` may overflow on huge inputs.",
        "low",
        SUM_LINE,
    );
    let result = review_payload(
        "One note on the total.",
        "APPROVE*",
        "C+",
        serde_json::json!([note]),
        "UNVERIFIABLE",
    )
    .await;
    let (verdict, fails, json) = run_json(&result);
    assert_eq!(
        verdict,
        Verdict::ApproveWithReservations,
        "{:?}",
        result.error
    );
    assert!(
        !fails,
        "an all-withheld APPROVE* exits 0: {:?}",
        result.error
    );
    assert_eq!(json["verdict"], "APPROVE*");
    assert_eq!(json["verdict_status"], "all_withheld", "{json}"); // #9310
    assert_eq!(json["grade"], "C+");
    assert_eq!(json["withheld_by_reason"]["unverifiable"], 1, "{json}");
    assert!(result.findings.is_empty(), "{:?}", result.findings);
}

/// AQ-7t counterpart: a REQUEST_CHANGES review whose only finding was withheld
/// never approves. #9310 (Architect ruling 2026-10-06 16:50Z): it is
/// REQUEST_CHANGES with `verdict_status: suppressed_reject`, no longer
/// UNKNOWN; it carries no error, so `run --json` exits as any REQUEST_CHANGES
/// review does, and its grade is the REQUEST_CHANGES band's best (`D+`).
/// The short quote reaches the #8905 gate; a longer absent quote is dropped
/// before grading
/// (`run_review_blocking_review_wiped_before_grading_is_suppressed_reject`).
#[tokio::test]
async fn run_review_all_withheld_request_changes_is_suppressed_reject() {
    let bug = billing_finding(
        "data-loss",
        "`flush_all()` loses the total.",
        "high",
        SUM_LINE,
    );
    let result = review_payload(
        "The flush loses data.",
        "REQUEST_CHANGES",
        "D",
        serde_json::json!([bug]),
        "CONFIRMED",
    )
    .await;
    let (verdict, fails, json) = run_json(&result);
    assert_eq!(verdict, Verdict::RequestChanges, "{json}");
    assert!(!fails, "no error is recorded: {:?}", result.error);
    assert_eq!(json["grade"], "D+", "{json}");
    assert_eq!(json["verdict_status"], "suppressed_reject", "{json}");
    assert!(result.findings.is_empty(), "{:?}", result.findings);
}

/// #9188 (Architect ruling, option A), single-pass path: a REQUEST_CHANGES
/// review whose only finding quotes code absent from the diff loses it before
/// grading. #4042 relaxes the verdict to APPROVE, but the final check decides
/// from the model's verdict. #9310: REQUEST_CHANGES, `suppressed_reject`,
/// never APPROVE and no longer UNKNOWN.
#[tokio::test]
async fn run_review_blocking_review_wiped_before_grading_is_suppressed_reject() {
    let bug = billing_finding(
        "data-loss",
        "`ledger.flush_all()` loses the total.",
        "high",
        SUM_LINE,
    );
    let result = review_payload(
        "The flush loses data.",
        "REQUEST_CHANGES",
        "D",
        serde_json::json!([bug]),
        "CONFIRMED",
    )
    .await;
    let (verdict, _fails, json) = run_json(&result);
    assert_eq!(verdict, Verdict::RequestChanges, "{json}");
    assert_eq!(json["grade"], "D+", "{json}");
    assert_eq!(
        json["withheld_by_reason"]["citation_integrity"], 1,
        "{json}"
    );
    assert_eq!(json["verdict_status"], "suppressed_reject", "{json}");
}

/// #9188 B, end to end: a CONFIRMED finding with one quoted snippet absent
/// from the file is withheld. The citation gate drops it before the verifier
/// runs, so this exercises B, not L; L's tests are the `withhold_unresolved_*`
/// tests in `withheld_contract_tests.rs`.
#[tokio::test]
async fn a_confirmed_finding_with_an_absent_quote_is_withheld() {
    let finding = billing_finding(
        "overflow",
        "`amounts.iter().sum::<u64>()` overflows before `ledger.reconcile_all()` runs.",
        "medium",
        SUM_LINE,
    );
    let result = review_payload(
        "Overflow risk.",
        "REQUEST_CHANGES",
        "C",
        serde_json::json!([finding]),
        "CONFIRMED",
    )
    .await;
    assert!(result.findings.is_empty(), "{:?}", result.findings);
    assert_ne!(result.verdict, Verdict::Approve);
}

/// A diff that deletes the admin check from `delete_account`; the deletion
/// sits at new-side line [`DELETION_LINE`].
fn removal_diff() -> &'static str {
    "diff --git a/src/admin.rs b/src/admin.rs\n--- a/src/admin.rs\n+++ b/src/admin.rs\n@@ -10,4 +10,3 @@\n fn delete_account(user: &User, id: u64) -> Result<()> {\n-    require_admin(user)?;\n     accounts::delete(id)?;\n     Ok(())\n"
}

/// The new-side position of the deleted `require_admin(user)?;` line.
const DELETION_LINE: u32 = 11;

/// #9188 F, end to end: a CONFIRMED finding about a removal, quoting the
/// removed code at its deletion's position, is posted. On 0baeb71106 the gate
/// kept it as "re-anchored" to the line it already cited, the post-verifier
/// re-check (L) read that as unresolved, and the review turned UNKNOWN.
#[tokio::test]
async fn run_review_posts_a_confirmed_removal_finding_at_its_deletion_line() {
    let finding = serde_json::json!({
        "title": "missing-admin-check",
        "body": "This removes `require_admin(user)?;`, so any user can delete any account.",
        "severity": "high",
        "confidence": 0.9,
        "file": "src/admin.rs",
        "line": DELETION_LINE,
    });
    let result = review_diff_payload(
        removal_diff(),
        "The admin check is gone from delete_account.",
        "REQUEST_CHANGES",
        "D",
        serde_json::json!([finding]),
        "CONFIRMED",
    )
    .await;
    assert_eq!(result.findings.len(), 1, "{:?}", result.withheld_findings);
    assert_eq!(result.findings[0].line, Some(DELETION_LINE));
    assert_ne!(result.verdict, Verdict::Unknown, "{:?}", result.error);
}

/// Row 5: a dropped finding's citation reaches the posted body by no route —
/// neither the reviewer's prose nor the fenced findings JSON.
#[tokio::test]
async fn run_review_body_carries_no_dropped_citation() {
    let result = review_one("This total can overflow on large invoices.", SUM_LINE).await;
    assert!(result.findings.is_empty());
    assert!(
        !result.review_body.contains("src/billing.rs"),
        "{}",
        result.review_body
    );
}

/// Prose that names locations no finding backs and that are not diff
/// citations: an address, a host, a path outside the diff, and a range whose
/// start is not the survivor's line (MEDIUM-1 on #9188).
const CLEAN_PROSE: &str = "Overflow risk at src/billing.rs:29-31 on large invoices. The billing daemon binds 127.0.0.1:8080, calls example.com:443, and is configured in config/app.toml:12.";

/// #9188 compatibility: a review with nothing withheld keeps the model's
/// prose and findings byte for byte through the real pipeline.
#[tokio::test]
async fn run_review_keeps_a_clean_review_byte_for_byte() {
    let body = "`amounts.iter().sum::<u64>()` can overflow on large invoices.";
    let finding = billing_finding("overflow", body, "medium", SUM_LINE);
    let result = review_payload(
        CLEAN_PROSE,
        "REQUEST_CHANGES",
        "C",
        serde_json::json!([finding]),
        "CONFIRMED",
    )
    .await;
    assert!(
        result.withheld_findings.is_empty(),
        "{:?}",
        result.withheld_findings
    );
    // The model's prose verbatim, then its fenced payload (findings array
    // stripped, #8905 row 5), as before #9188; no rebuilt summary.
    let body_prefix = format!("{CLEAN_PROSE}\n\n```json\n");
    assert!(
        result.review_body.starts_with(&body_prefix),
        "{}",
        result.review_body
    );
    assert!(
        !result.review_body.contains("withheld"),
        "{}",
        result.review_body
    );
    assert_eq!(result.findings.len(), 1);
    let f = &result.findings[0];
    assert_eq!(
        (
            f.file.as_str(),
            f.line,
            f.kind.as_str(),
            f.description.as_str()
        ),
        ("src/billing.rs", Some(SUM_LINE), "overflow", body)
    );
    assert_eq!(f.citation_correction, None);
}
