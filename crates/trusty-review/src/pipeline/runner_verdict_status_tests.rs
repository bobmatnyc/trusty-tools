//! End-to-end regressions for #9310 part 2: `verdict_status` names every
//! review outcome, the withheld headline counts the whole array, and a
//! tool-call reply is parsed from its tool input only.
//!
//! Why: on `origin/main` a clean review carried no status, a parse failure
//! and a transport error read the same UNKNOWN, the headline counted one
//! stage's drops (6) while `withheld_findings` held 10, and a tool-call reply
//! whose input did not deserialize was parsed from a ```json fence after it.
//! What: drives `run_review` and reads the `run --json` payload, so each
//! assertion is on what a caller sees.
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

/// A reviewer that answers through a tool call: Bedrock's `tool_use` stop
/// reason, with `text` as the reply text the provider hands the parser.
struct ToolCallLlm {
    text: String,
}

#[async_trait]
impl LlmProvider for ToolCallLlm {
    fn name(&self) -> &str {
        "fake-tool-call"
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            text: self.text.clone(),
            model: req.model.clone(),
            input_tokens: 100,
            output_tokens: 50,
            latency_ms: 42,
            cost_usd: 0.000042,
            finish_reason: Some("tool_use".to_string()),
        })
    }
}

/// Review [`billing_diff`] with `llm`, a verifier answering CONFIRMED.
async fn review_with(llm: Arc<dyn LlmProvider>) -> ReviewResult {
    let (source, _tmp) = local_diff_source(&billing_diff());
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
    run_review(&default_config(), input, ready_deps(llm, Some(verifier))).await
}

/// A text reviewer returning `response` verbatim.
fn text_llm(response: &str) -> Arc<dyn LlmProvider> {
    Arc::new(FakeLlm {
        response: response.to_string(),
        error: None,
        output_tokens: None,
    })
}

/// The `run --json` payload of `result`.
fn json_of(result: &ReviewResult) -> serde_json::Value {
    crate::run_output::run_json_payload(result)
}

/// #9310 item 1: a clean review reads `parsed` and APPROVE, never UNKNOWN.
#[tokio::test]
async fn a_clean_review_reads_parsed_approve() {
    let json = json_of(&review_with(Arc::new(FakeLlm::approves())).await);
    assert_eq!(json["verdict"], "APPROVE", "{json}");
    assert_eq!(json["verdict_status"], "parsed", "{json}");
}

/// #9310 item 1: a reply that does not parse reads `parse_failed`, UNKNOWN,
/// and keeps the parse-failure reason in `error`.
#[tokio::test]
async fn an_unparsed_reply_reads_parse_failed() {
    let result = review_with(text_llm("The change looks fine to me.")).await;
    let json = json_of(&result);
    assert_eq!(json["verdict"], "UNKNOWN", "{json}");
    assert_eq!(json["verdict_status"], "parse_failed", "{json}");
    let error = result.error.unwrap_or_default();
    assert!(error.starts_with("review not parsed"), "{error}");
}

/// #9310 item 1: no reviewer reply — a transport error, an empty reply, or a
/// reply cut at the token ceiling — reads `no_reviewer_output` and UNKNOWN.
#[tokio::test]
async fn no_reviewer_reply_reads_no_reviewer_output() {
    let failing = FakeLlm {
        response: String::new(),
        error: Some("connection refused".to_string()),
        output_tokens: None,
    };
    let cases: [(&str, Arc<dyn LlmProvider>); 3] = [
        ("transport error", Arc::new(failing)),
        ("empty reply", text_llm("  \n")),
        ("truncated", Arc::new(FakeLlm::truncated_at_ceiling())),
    ];
    for (case, llm) in cases {
        let json = json_of(&review_with(llm).await);
        assert_eq!(json["verdict"], "UNKNOWN", "{case}: {json}");
        assert_eq!(
            json["verdict_status"], "no_reviewer_output",
            "{case}: {json}"
        );
    }
}

/// #9310 item 3, the #9306 shape: four findings dropped before grading
/// (quoted code absent from the diff) and six dropped by the line-citation
/// gate. `origin/main` led the body with "6 findings withheld: citation
/// unverifiable" while `withheld_findings` held 10; the headline now reads
/// the array.
#[tokio::test]
async fn the_withheld_headline_counts_the_whole_array() {
    let mut findings = Vec::new();
    for i in 0..4 {
        findings.push(serde_json::json!({
            "title": format!("lost-write-{i}"),
            "body": format!("`ledger.flush_all_{i}()` loses the total."),
            "severity": "low",
            "confidence": 0.9,
            "file": "src/billing.rs",
            "line": SUM_LINE,
        }));
    }
    for i in 0..6 {
        findings.push(serde_json::json!({
            "title": format!("style-{i}"),
            "body": format!("`flush_{i}()` is slow."),
            "severity": "low",
            "confidence": 0.9,
            "file": "src/billing.rs",
            "line": SUM_LINE,
        }));
    }
    let payload = serde_json::json!({
        "verdict": "APPROVE",
        "summary": "Minor notes only.",
        "findings": findings,
    });
    let result = review_with(text_llm(&format!(
        "Minor notes only.\n\n```json\n{payload}\n```"
    )))
    .await;
    let json = json_of(&result);
    assert_eq!(json["withheld_count"], 10, "{json}");
    let headline = result.review_body.lines().next().unwrap_or_default();
    assert!(
        headline.starts_with("10 findings withheld"),
        "the headline must count all 10 withheld findings: {}",
        result.review_body
    );
    // #9310 item 2: the model approved, so an all-withheld review stays APPROVE.
    assert_eq!(json["verdict"], "APPROVE", "{json}");
    assert_eq!(json["verdict_status"], "all_withheld", "{json}");
}

/// #9310 item 4: a tool-call reply is parsed from its tool input only. The
/// input here lacks a finding `body`, so it does not deserialize; the valid
/// ```json review after it is reply text the tool call did not carry, and
/// `origin/main` parsed it as APPROVE.
#[tokio::test]
async fn a_tool_call_reply_never_parses_a_json_fence() {
    let text = "{\"verdict\":\"APPROVE\",\"summary\":\"ok\",\"findings\":[{\"title\":\"t\"}]}\n\n\
                ```json\n{\"verdict\":\"APPROVE\",\"summary\":\"ok\",\"findings\":[]}\n```";
    let result = review_with(Arc::new(ToolCallLlm {
        text: text.to_string(),
    }))
    .await;
    let json = json_of(&result);
    assert_eq!(json["verdict"], "UNKNOWN", "{json}");
    assert_eq!(json["verdict_status"], "parse_failed", "{json}");
}

/// #9310 item 4: a tool input that fails to deserialize is a parse failure
/// naming the serde cause, with no keyword scan of the input's text.
#[tokio::test]
async fn a_tool_call_parse_failure_runs_no_keyword_scan() {
    let text = r#"{"verdict":"APPROVE","summary":"ok","findings":[{"title":"t"}]}"#;
    let result = review_with(Arc::new(ToolCallLlm {
        text: text.to_string(),
    }))
    .await;
    let json = json_of(&result);
    assert_eq!(json["verdict"], "UNKNOWN", "{json}");
    assert_eq!(json["verdict_status"], "parse_failed", "{json}");
    let error = result.error.unwrap_or_default();
    assert!(error.contains("missing field `body`"), "{error}");
    assert!(!error.contains("keyword scan"), "{error}");
}

/// #9310 item 4: a valid tool input parses, so a tool-call review is judged.
#[tokio::test]
async fn a_valid_tool_call_reply_parses() {
    let text = r#"{"verdict":"APPROVE","summary":"Clean change.","findings":[]}"#;
    let json = json_of(
        &review_with(Arc::new(ToolCallLlm {
            text: text.to_string(),
        }))
        .await,
    );
    assert_eq!(json["verdict"], "APPROVE", "{json}");
    assert_eq!(json["verdict_status"], "parsed", "{json}");
}
