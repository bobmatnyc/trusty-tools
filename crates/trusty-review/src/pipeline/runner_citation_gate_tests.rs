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
    run_review(&default_config(), input, ready_deps(Arc::new(llm), None)).await
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
    assert_eq!(result.verdict, Verdict::Unknown);
    assert!(
        result
            .review_body
            .starts_with("1 findings withheld: citation unverifiable")
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
    assert_eq!(result.verdict, Verdict::Unknown);
    assert!(
        result
            .review_body
            .starts_with("1 findings withheld: citation unverifiable")
    );
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
