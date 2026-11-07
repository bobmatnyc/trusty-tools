//! End-to-end regressions for #10782: a finding carries a quote, a note with
//! nothing to quote is withheld before the gate and counted, and `questions`
//! never shape the verdict.
//!
//! Why: on trusty-review 0.38.5 the reviewer's "confirm X" notes reached the
//! line-citation gate as findings, so an APPROVE ended `all_withheld` with
//! every drop counted as `line_citation`. These drive `run_review` and read
//! the `run --json` payload a caller sees.
//! Test: this module.

use super::*;

/// The line of `src/billing.rs` that holds `amounts.iter().sum::<u64>()`.
const SUM_LINE: u32 = 30;

/// The code on [`SUM_LINE`], as the reviewer would quote it.
const SUM_CODE: &str = "let total = amounts.iter().sum::<u64>();";

/// A 40-line new file whose line [`SUM_LINE`] sums the amounts.
fn billing_diff() -> String {
    let mut diff = String::from(
        "diff --git a/src/billing.rs b/src/billing.rs\n--- a/src/billing.rs\n+++ b/src/billing.rs\n@@ -0,0 +1,40 @@\n",
    );
    for i in 1..=40 {
        if i == SUM_LINE {
            diff.push_str(&format!("+    {SUM_CODE}\n"));
        } else {
            diff.push_str(&format!("+    let value_{i} = step_{i}(input);\n"));
        }
    }
    diff
}

/// A finding at [`SUM_LINE`] with `body` and an optional structured quote.
fn finding(title: &str, body: &str, quote: Option<&str>) -> serde_json::Value {
    let mut f = serde_json::json!({
        "title": title,
        "body": body,
        "severity": "medium",
        "confidence": 0.3,
        "file": "src/billing.rs",
        "line": SUM_LINE,
        "code_provable": false,
    });
    if let Some(q) = quote {
        f["quoted_snippet"] = serde_json::Value::String(q.to_string());
    }
    f
}

/// Review [`billing_diff`] with a reviewer replying `payload` as a JSON
/// fence, and a verifier that confirms every finding.
async fn review(payload: serde_json::Value) -> ReviewResult {
    let (source, _tmp) = local_diff_source(&billing_diff());
    let llm = FakeLlm {
        response: format!("Review.\n\n```json\n{payload}\n```"),
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

fn json_of(result: &ReviewResult) -> serde_json::Value {
    crate::run_output::run_json_payload(result)
}

/// #10782 closure 1: a posted finding carries its structured quote, and its
/// body need quote nothing.
#[tokio::test]
async fn run_review_posts_a_finding_with_its_quoted_snippet() {
    let result = review(serde_json::json!({
        "verdict": "REQUEST_CHANGES",
        "grade": "C",
        "summary": "overflow",
        "findings": [finding("overflow", "The total can overflow on large invoices.", Some(SUM_CODE))],
    }))
    .await;
    let json = json_of(&result);
    assert_eq!(result.findings.len(), 1, "{json}");
    assert_eq!(json["findings"][0]["quoted_snippet"], SUM_CODE, "{json}");
    assert_eq!(json["findings"][0]["line"], SUM_LINE, "{json}");
    assert!(json.get("withheld_by_reason").is_none(), "{json}");
}

/// #10782 closures 2-3, the duetto#11211 shape: an APPROVE whose every
/// finding is a "confirm X" note with nothing to quote. The notes are
/// withheld before the gate under `no_quote_pregate`, never `line_citation`.
/// Architect ruling 09:02Z: pregate-only drops are not `all_withheld`; the
/// review is a `parsed` APPROVE flagged `pregate_only_approve`.
#[tokio::test]
async fn run_review_pregate_only_approve_is_parsed_not_all_withheld() {
    let result = review(serde_json::json!({
        "verdict": "APPROVE",
        "grade": "A",
        "summary": "Looks fine.",
        "findings": [
            finding("confirm", "Confirm getMetricContext is imported in context.ts.", None),
            finding("check", "Check that the caller handles an empty list.", Some("  ")),
            finding("verify", "Verify `step_4` is awaited.", None),
        ],
    }))
    .await;
    let json = json_of(&result);
    assert!(result.findings.is_empty(), "{json}");
    assert_eq!(json["withheld_count"], 3, "{json}");
    assert_eq!(
        json["withheld_by_reason"],
        serde_json::json!({"no_quote_pregate": 3}),
        "{json}"
    );
    assert_eq!(json["verdict"], "APPROVE", "{json}");
    assert_eq!(json["verdict_status"], "parsed", "{json}");
    assert_eq!(json["pregate_only_approve"], true, "{json}");
}

/// #10782 ruling 09:02Z, unchanged arm: an APPROVE whose only withhold is a
/// gate withhold (`line_citation`) keeps `all_withheld`, and is not flagged.
#[tokio::test]
async fn run_review_gate_only_all_withheld_approve_is_unchanged() {
    let result = review(serde_json::json!({
        "verdict": "APPROVE",
        "grade": "A",
        "summary": "Looks fine.",
        "findings": [finding("lost-write", "The ledger write is lost.", Some("ledger.flush_all().await?;"))],
    }))
    .await;
    let json = json_of(&result);
    assert!(result.findings.is_empty(), "{json}");
    assert_eq!(
        json["withheld_by_reason"],
        serde_json::json!({"line_citation": 1}),
        "{json}"
    );
    assert_eq!(json["verdict"], "APPROVE", "{json}");
    assert_eq!(json["verdict_status"], "all_withheld", "{json}");
    assert!(json.get("pregate_only_approve").is_none(), "{json}");
}

/// #10782 ruling 09:02Z, the mixed rule: pregate drops beside any gate
/// withhold, with nothing surviving, still count as `all_withheld`.
#[tokio::test]
async fn run_review_mixed_pregate_and_gate_withholds_are_all_withheld() {
    let result = review(serde_json::json!({
        "verdict": "APPROVE",
        "grade": "A",
        "summary": "Looks fine.",
        "findings": [
            finding("confirm", "Confirm the flush is awaited.", None),
            finding("lost-write", "The ledger write is lost.", Some("ledger.flush_all().await?;")),
        ],
    }))
    .await;
    let json = json_of(&result);
    assert!(result.findings.is_empty(), "{json}");
    assert_eq!(
        json["withheld_by_reason"],
        serde_json::json!({"line_citation": 1, "no_quote_pregate": 1}),
        "{json}"
    );
    assert_eq!(json["verdict_status"], "all_withheld", "{json}");
    assert!(json.get("pregate_only_approve").is_none(), "{json}");
}

/// #10782 keeps #9188 A / #9310: a blocking review whose every finding is
/// withheld before the gate is never APPROVE — it is `suppressed_reject`,
/// whatever the withhold reason, and is not flagged `pregate_only_approve`.
#[tokio::test]
async fn run_review_blocking_review_of_no_quote_notes_is_suppressed_reject() {
    let result = review(serde_json::json!({
        "verdict": "REQUEST_CHANGES",
        "grade": "D",
        "summary": "Unsure.",
        "findings": [finding("confirm", "Confirm the flush is awaited.", None)],
    }))
    .await;
    let json = json_of(&result);
    assert!(result.findings.is_empty(), "{json}");
    assert_eq!(json["verdict"], "REQUEST_CHANGES", "{json}");
    assert_eq!(json["verdict_status"], "suppressed_reject", "{json}");
    assert_eq!(json["withheld_by_reason"]["no_quote_pregate"], 1, "{json}");
    assert!(json.get("pregate_only_approve").is_none(), "{json}");
}

/// #10782 error arm: a structured quote that is not in the cited file is
/// withheld by the gate, never posted, and counted.
#[tokio::test]
async fn run_review_a_mismatched_quote_is_withheld() {
    let result = review(serde_json::json!({
        "verdict": "REQUEST_CHANGES",
        "grade": "C",
        "summary": "lost write",
        "findings": [finding("lost-write", "The ledger write is lost.", Some("ledger.flush_all().await?;"))],
    }))
    .await;
    let json = json_of(&result);
    assert!(result.findings.is_empty(), "{json}");
    assert_eq!(json["withheld_by_reason"]["line_citation"], 1, "{json}");
    assert_eq!(
        result.withheld_findings[0].missing_fragment.as_deref(),
        Some("ledger.flush_all().await?;")
    );
}

/// #10782 closure 2: `questions` reach the result, and no gate, grade or
/// verdict reads them — the review is the same clean APPROVE without them.
#[tokio::test]
async fn run_review_questions_never_shape_the_verdict() {
    let with = review(serde_json::json!({
        "verdict": "APPROVE",
        "grade": "A",
        "summary": "Clean.",
        "questions": ["Confirm getMetricContext is imported in context.ts."],
        "findings": [],
    }))
    .await;
    let without = review(serde_json::json!({
        "verdict": "APPROVE",
        "grade": "A",
        "summary": "Clean.",
        "findings": [],
    }))
    .await;
    let (json, plain) = (json_of(&with), json_of(&without));
    assert_eq!(
        json["questions"],
        serde_json::json!(["Confirm getMetricContext is imported in context.ts."]),
        "{json}"
    );
    assert!(plain.get("questions").is_none(), "{plain}");
    for key in ["verdict", "verdict_status", "grade", "withheld_count"] {
        assert_eq!(json[key], plain[key], "{key}: {json}");
    }
    assert_eq!(json["verdict_status"], "parsed", "{json}");
}

/// Collects formatted log output (the `runner_credential_url_tests` pattern).
#[derive(Clone, Default)]
struct Capture(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("capture lock").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// #10782 ruling 09:02Z: one info event per review carries the pregate drop
/// count beside the gate-withhold count.
#[tokio::test]
async fn run_review_logs_the_pregate_drop_count_once() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(capture.clone())
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .finish();
    let _result = {
        // A current-thread runtime runs every task on this thread.
        let _guard = tracing::subscriber::set_default(subscriber);
        review(serde_json::json!({
            "verdict": "APPROVE",
            "grade": "A",
            "summary": "Looks fine.",
            "findings": [
                finding("confirm", "Confirm the flush is awaited.", None),
                finding("check", "Check that the caller handles an empty list.", None),
                finding("lost-write", "The ledger write is lost.", Some("ledger.flush_all().await?;")),
            ],
        }))
        .await
    };
    let log = String::from_utf8(capture.0.lock().expect("capture lock").clone()).expect("utf8");
    let events: Vec<&str> = log
        .lines()
        .filter(|l| l.contains("pregate drop count for this review"))
        .collect();
    assert_eq!(events.len(), 1, "{log}");
    let event = events[0];
    assert!(event.contains(" INFO "), "{event}");
    assert!(event.contains("pregate_withheld=2"), "{event}");
    assert!(event.contains("gate_withheld=1"), "{event}");
    assert!(event.contains("pregate_only_approve=false"), "{event}");
}
