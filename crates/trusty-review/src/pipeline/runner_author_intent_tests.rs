//! End-to-end regression for #10758: a declared as-is migration's carried-over
//! legacy style is withheld, and a bug the change introduces is still reported.
//!
//! Why: the reviewer flagged deliberate verbatim moves of legacy code as
//! defects. These drive `run_review` with a fake reviewer and verifier (no
//! live model) over a move diff, with and without author intent.
//! Test: this module.

use super::*;

/// Legacy lines the migration moves verbatim from `old/` to `new/`.
const LEGACY_A: &str = "var result = fetchData(url, null, null, false);";
const LEGACY_B: &str = "if (result == undefined) { return -1; }";
/// The one line the migration changed: it drops the `await`.
const INTRODUCED: &str = "const rows = db.query(sql);";
const OLD_LINE: &str = "const rows = await db.query(sql);";

/// Removes `old/legacy.js`, adds `new/legacy.js` with the same legacy lines
/// and one altered line.
fn migration_diff() -> String {
    format!(
        "diff --git a/old/legacy.js b/old/legacy.js\n--- a/old/legacy.js\n+++ /dev/null\n\
         @@ -1,3 +0,0 @@\n-{LEGACY_A}\n-{LEGACY_B}\n-{OLD_LINE}\n\
         diff --git a/new/legacy.js b/new/legacy.js\n--- /dev/null\n+++ b/new/legacy.js\n\
         @@ -0,0 +1,3 @@\n+{LEGACY_A}\n+{LEGACY_B}\n+{INTRODUCED}\n"
    )
}

fn finding(title: &str, line: u32, quote: &str) -> serde_json::Value {
    serde_json::json!({
        "title": title,
        "body": format!("`{quote}` is a problem."),
        "severity": "medium",
        "confidence": 0.8,
        "file": "new/legacy.js",
        "line": line,
        "code_provable": true,
        "quoted_snippet": quote,
    })
}

async fn review(intent: Option<&str>) -> ReviewResult {
    let payload = serde_json::json!({
        "verdict": "REQUEST_CHANGES",
        "grade": "C",
        "summary": "Legacy style and a missing await.",
        "findings": [
            finding("legacy-null-args", 1, LEGACY_A),
            finding("loose-equality", 2, LEGACY_B),
            finding("missing-await", 3, INTRODUCED),
        ],
    });
    let (source, _tmp) = local_diff_source(&migration_diff());
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
        caller_context: CallerContext {
            author_intent: intent.map(str::to_string),
            ..CallerContext::default()
        },
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

fn kinds(result: &ReviewResult) -> Vec<&str> {
    result.findings.iter().map(|f| f.kind.as_str()).collect()
}

/// #10758: with as-is intent, the two carried-over legacy findings are
/// withheld as `as_is_carryover`; the introduced missing-await bug is kept.
#[tokio::test]
async fn run_review_as_is_intent_withholds_legacy_style_keeps_introduced_bug() {
    let result = review(Some("As-is legacy migration: moved verbatim.")).await;
    let json = crate::run_output::run_json_payload(&result);
    assert_eq!(kinds(&result), vec!["missing-await"], "{json}");
    assert_eq!(
        json["withheld_by_reason"],
        serde_json::json!({"as_is_carryover": 2}),
        "{json}"
    );
}

/// #10758 criterion 4: without intent, the review keeps all three findings.
#[tokio::test]
async fn run_review_without_intent_keeps_every_finding() {
    let result = review(None).await;
    let json = crate::run_output::run_json_payload(&result);
    assert_eq!(
        kinds(&result),
        vec!["legacy-null-args", "loose-equality", "missing-await"],
        "{json}"
    );
}
