//! Unit tests for `tools::recall_session` — split out of `recall_session.rs`
//! per the crate's `_tests.rs` sibling-file convention (see
//! `events`/`events_tests` and `session::registry`/`registry_tests` for
//! precedent) so the tool's growing test surface (DOC-39 Slice C added
//! `text`/`run_id` coverage) doesn't push the production file past its
//! 500-SLOC cap — test files carry the 1500-SLOC cap.
//! What: covers `filter_and_cap`, `render_results`, `recall_telemetry`
//! (including DOC-39 Slice C's `text`/`run_id` extraction), the tool's
//! schema, and `execute` end-to-end against an in-process mock trusty-memory
//! `/rpc` server.
//! Test: this module is itself the test surface.

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use tokio::net::TcpListener;

use super::*;

/// A `memory_recall` result entry with `content`/`score`/`tags`.
fn result_entry(content: &str, score: f64, tags: &[&str]) -> Value {
    json!({
        "drawer_id": "00000000-0000-0000-0000-000000000000",
        "content": content,
        "score": score,
        "layer": 1,
        "tags": tags,
        "importance": 0.5,
        "drawer_type": "session_note",
    })
}

/// Wrap a `serialize_recall`-shaped body as the real spike-verified
/// `tools/call` envelope: the inner JSON STRINGIFIED inside
/// `content[0].text`.
fn tools_call_envelope(palace: &str, query: &str, results: Vec<Value>) -> Value {
    let inner = json!({"palace": palace, "query": query, "results": results}).to_string();
    json!({"content": [{"type": "text", "text": inner}]})
}

// NOTE (#2424): the envelope-unwrap unit tests moved with the helper to
// `crate::memory_envelope::tests` when `parse_recall_envelope` was
// promoted to the shared `parse_tools_call_envelope`.

// ── `filter_and_cap` ─────────────────────────────────────────────────

#[test]
fn filter_and_cap_keeps_only_tagged_results_in_order() {
    let results = vec![
        result_entry("a", 0.9, &["session:s1", "turn"]),
        result_entry("b", 0.8, &["session:other", "turn"]),
        result_entry("c", 0.7, &["session:s1", "turn"]),
    ];
    let filtered = filter_and_cap(&results, "session:s1", 10);
    assert_eq!(filtered.len(), 2);
    assert_eq!(filtered[0]["content"], "a");
    assert_eq!(filtered[1]["content"], "c");
}

#[test]
fn filter_and_cap_respects_top_k() {
    let results = vec![
        result_entry("a", 0.9, &["session:s1"]),
        result_entry("b", 0.8, &["session:s1"]),
        result_entry("c", 0.7, &["session:s1"]),
    ];
    let filtered = filter_and_cap(&results, "session:s1", 2);
    assert_eq!(filtered.len(), 2);
    assert_eq!(filtered[0]["content"], "a");
    assert_eq!(filtered[1]["content"], "b");
}

// ── `render_results` ─────────────────────────────────────────────────

#[test]
fn render_drops_whole_lowest_scored_entries_over_budget() {
    // Each "x" repeated 4 chars ~ 1 token by the chars/4 heuristic.
    // Build one huge entry that alone exceeds the budget, followed by a
    // small one; expect the small one dropped, not mid-text truncated.
    let huge = "x".repeat((TOKEN_BUDGET + 100) * 4);
    let results = vec![result_entry(&huge, 0.9, &["session:s1"]), {
        let mut r = result_entry("small tail entry", 0.8, &["session:s1"]);
        r["content"] = json!("small tail entry");
        r
    }];
    let (rendered, injected_count) = render_results("q", &results);
    assert!(
        rendered.contains(&huge),
        "first entry always included whole"
    );
    assert!(
        !rendered.contains("small tail entry"),
        "second (lower-scored) entry must be dropped whole, not merged/truncated in"
    );
    assert_eq!(injected_count, 1, "only the first entry entered context");
}

#[test]
fn render_includes_all_entries_within_budget() {
    let results = vec![
        result_entry("first", 0.9, &["session:s1"]),
        result_entry("second", 0.8, &["session:s1"]),
    ];
    let (rendered, injected_count) = render_results("q", &results);
    assert!(rendered.contains("first"));
    assert!(rendered.contains("second"));
    assert_eq!(
        injected_count, 2,
        "both entries fit, so both entered context"
    );
}

// ── telemetry (UI Phase 1) ───────────────────────────────────────────

/// Extract the `RecallTelemetry` a result carries, or panic.
fn recall_telemetry_of(result: &ToolResult) -> &RecallTelemetry {
    match result.telemetry() {
        Some(ToolTelemetry::Recall(t)) => t,
        other => panic!("expected recall telemetry, got {other:?}"),
    }
}

/// A result the token budget dropped must be reported as recalled but
/// NOT injected — the UI's "41% · held" surface.
///
/// Why: THE point of `Event::MemoryRecalled`. The tool has always known
/// which results its budget dropped; before this ticket that knowledge
/// died inside the rendered text. `injected: false` must mean exactly
/// "recalled but not entered into context".
/// What: two results where the first alone busts the budget; assert the
/// second is present in the telemetry, flagged held-back, with its score.
/// Test: this test.
#[test]
fn telemetry_marks_budget_dropped_results_held_back() {
    let huge = "x".repeat((TOKEN_BUDGET + 100) * 4);
    let results = vec![
        result_entry(&huge, 0.9, &["session:s1"]),
        result_entry("held back", 0.41, &["session:s1"]),
    ];
    let (_text, injected_count) = render_results("q", &results);
    let telemetry = recall_telemetry("q", &results, injected_count);

    assert_eq!(
        telemetry.results,
        vec![
            RecalledMemory {
                score: 0.9,
                injected: true,
                text: huge.clone(),
                run_id: None,
            },
            RecalledMemory {
                score: 0.41,
                injected: false,
                text: "held back".to_string(),
                run_id: None,
            },
        ],
        "a budget-dropped result must still be REPORTED, flagged held-back \
         — dropping it from the telemetry too would hide it from the UI \
         entirely, which is the exact opposite of the requirement"
    );
}

/// The whole point of DOC-39 Slice C: a result the token budget dropped
/// must still carry its actual recalled TEXT in the telemetry, not just
/// a score — otherwise the "what was held back" UI surface can count a
/// held-back memory but never let an operator read it.
///
/// Why: `recall_telemetry` reads `text`/`run_id` from the SAME `content`
/// value `render_results` already parses (`result_entry`'s `content`
/// field). `run_id` is not part of the current daemon response shape, so
/// it must default to `None` without panicking, never `unwrap`.
/// What: two results where the first alone busts the budget; assert the
/// held-back second entry's telemetry carries its full text and a `None`
/// `run_id`, and the injected first entry carries its own text too.
/// Test: this test.
#[test]
fn telemetry_carries_recalled_text_and_run_id() {
    let huge = "x".repeat((TOKEN_BUDGET + 100) * 4);
    let results = vec![
        result_entry(&huge, 0.9, &["session:s1"]),
        result_entry("PKCE required for the OAuth flow", 0.41, &["session:s1"]),
    ];
    let (_text, injected_count) = render_results("q", &results);
    let telemetry = recall_telemetry("q", &results, injected_count);

    assert_eq!(telemetry.results.len(), 2);
    assert_eq!(telemetry.results[0].text, huge, "injected result's text");
    assert!(telemetry.results[0].injected);
    assert_eq!(telemetry.results[0].run_id, None);

    let held_back = &telemetry.results[1];
    assert!(!held_back.injected, "second entry must be held back");
    assert_eq!(
        held_back.text, "PKCE required for the OAuth flow",
        "the held-back result's TEXT must still be present — the whole \
         point of the 'what was held back' UI surface is reading it"
    );
    assert_eq!(
        held_back.run_id, None,
        "run_id defaults to None (never panics) when the daemon result \
         carries no such field"
    );
}

/// Every result that fit the budget must be reported injected.
#[test]
fn telemetry_marks_all_injected_when_within_budget() {
    let results = vec![
        result_entry("first", 0.9, &["session:s1"]),
        result_entry("second", 0.8, &["session:s1"]),
    ];
    let (_text, injected_count) = render_results("q", &results);
    let telemetry = recall_telemetry("q", &results, injected_count);

    assert!(
        telemetry.results.iter().all(|r| r.injected),
        "both fit, so both entered context: {:?}",
        telemetry.results
    );
    assert_eq!(telemetry.query, "q");
}

/// `execute` must attach telemetry whose injected flags agree with the
/// text it actually returned (UI Phase 1).
///
/// Why: an end-to-end guard that the render and the telemetry can never
/// disagree about what the model saw — the failure mode a UI could not
/// detect on its own.
/// Test: this test.
#[tokio::test]
async fn execute_attaches_telemetry_matching_the_rendered_text() {
    let results = vec![
        result_entry("visible one", 0.9, &["session:s1", "turn"]),
        result_entry("visible two", 0.7, &["session:s1", "turn"]),
    ];
    let (base_url, _captured) = spawn_recall_mock(results).await;
    let tool = RecallSessionTool::new("s1", base_url, "p");

    let result = tool.execute(json!({"query": "foo"})).await;
    let telemetry = recall_telemetry_of(&result);

    assert_eq!(telemetry.query, "foo");
    assert_eq!(telemetry.results.len(), 2);
    for (i, r) in telemetry.results.iter().enumerate() {
        assert!(r.injected, "result {i} is in the text, so it is injected");
    }
    assert!(result.content().contains("visible one"));
    assert!(result.content().contains("visible two"));
}

/// The fail-open paths must attach no telemetry — nothing was recalled.
///
/// Why: emitting a `MemoryRecalled` with an empty result set for an
/// unreachable daemon would tell the UI "we recalled nothing relevant"
/// when the truth is "we could not look". Those are different facts.
/// Test: this test.
#[tokio::test]
async fn fail_open_paths_report_no_telemetry() {
    let tool = RecallSessionTool::new("s1", "http://127.0.0.1:1", "p");
    let result = tool.execute(json!({"query": "anything"})).await;
    assert!(!result.is_error());
    assert!(
        result.telemetry().is_none(),
        "an unreachable daemon is not a recall with zero results"
    );
}

/// Dropping a lowest-scored result for budget emits an INFO-level log
/// naming the dropped/included counts (#2857).
///
/// Why: the budget truncation this module implements is exactly the
/// "recall_session: token-budget truncation dropping lowest-scored
/// results" site this ticket names as audited — the model silently
/// receives fewer memories than requested, which must be diagnosable
/// from stderr.
/// What: Same over-budget scenario as
/// `render_drops_whole_lowest_scored_entries_over_budget`, captured via
/// `crate::test_support::begin_capture`/`captured_at_least`.
/// Test: this test.
#[test]
fn render_drop_logs_info() {
    crate::test_support::begin_capture();

    let huge = "x".repeat((TOKEN_BUDGET + 100) * 4);
    let results = vec![
        result_entry(&huge, 0.9, &["session:s1"]),
        result_entry("small tail entry", 0.8, &["session:s1"]),
    ];
    render_results("q", &results);

    let captured = crate::test_support::captured_at_least(tracing::Level::INFO);
    assert!(
        captured.iter().any(|m| m.contains("token budget exceeded")),
        "expected an info-level budget-drop log, got: {captured:?}"
    );
}

// ── schema ───────────────────────────────────────────────────────────

#[test]
fn schema_has_required_fields() {
    let tool = RecallSessionTool::new("s1", "http://x", "p");
    let schema = tool.schema();
    let params = &schema["function"]["parameters"];
    assert_eq!(schema["function"]["name"], RECALL_SESSION_TOOL_NAME);
    let required: Vec<&str> = params["required"]
        .as_array()
        .expect("required array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(required, vec!["query"]);
    assert_eq!(params["additionalProperties"], json!(false));
}

// ── `execute` end-to-end against a mock daemon ───────────────────────

/// One captured `/rpc` call: JSON-RPC `method` + `params`.
type Captured = Arc<Mutex<Vec<(String, Value)>>>;

/// Spin up a mock `/rpc` server that always replies with the
/// spike-verified `tools/call` envelope shape for `memory_recall`,
/// capturing every request's method/params.
async fn spawn_recall_mock(results: Vec<Value>) -> (String, Captured) {
    let captured: Captured = Arc::new(Mutex::new(Vec::new()));
    let store = Arc::clone(&captured);
    let results_state = Arc::new(results);

    async fn handle(
        State((store, results)): State<(Captured, Arc<Vec<Value>>)>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let method = body["method"].as_str().unwrap_or_default().to_string();
        let params = body["params"].clone();
        store
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((method, params.clone()));
        let palace = params["arguments"]["palace"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let query = params["arguments"]["query"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let envelope = tools_call_envelope(&palace, &query, (*results).clone());
        Json(json!({"jsonrpc": "2.0", "id": 1, "result": envelope}))
    }

    let app = Router::new()
        .route("/rpc", post(handle))
        .with_state((store, results_state));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), captured)
}

#[tokio::test]
async fn execute_returns_only_session_tagged_results() {
    let results = vec![
        result_entry("mine: file is at src/foo.rs", 0.9, &["session:s1", "turn"]),
        result_entry("other session's note", 0.8, &["session:s2", "turn"]),
        result_entry("mine again", 0.7, &["session:s1", "turn"]),
    ];
    let (base_url, _captured) = spawn_recall_mock(results).await;
    let tool = RecallSessionTool::new("s1", base_url, "p");

    let result = tool.execute(json!({"query": "foo"})).await;
    assert!(!result.is_error());
    assert!(result.content().contains("src/foo.rs"));
    assert!(result.content().contains("mine again"));
    assert!(!result.content().contains("other session's note"));
}

#[tokio::test]
async fn execute_over_fetches_top_k_times_factor() {
    let (base_url, captured) = spawn_recall_mock(vec![]).await;
    let tool = RecallSessionTool::new("s1", base_url, "p");

    let _ = tool.execute(json!({"query": "foo", "top_k": 3})).await;

    let calls = captured.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(calls.len(), 1);
    let (method, params) = &calls[0];
    assert_eq!(method, "tools/call");
    assert_eq!(params["name"], "memory_recall");
    assert_eq!(params["arguments"]["top_k"], json!(3 * OVER_FETCH_FACTOR));
}

#[tokio::test]
async fn execute_clamps_top_k_to_max() {
    let (base_url, captured) = spawn_recall_mock(vec![]).await;
    let tool = RecallSessionTool::new("s1", base_url, "p");

    let _ = tool.execute(json!({"query": "foo", "top_k": 999})).await;

    let calls = captured.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(
        calls[0].1["arguments"]["top_k"],
        json!(MAX_TOP_K * OVER_FETCH_FACTOR)
    );
}

#[tokio::test]
async fn execute_is_fail_open_on_unreachable_daemon() {
    let tool = RecallSessionTool::new("s1", "http://127.0.0.1:1", "p");
    let result = tool.execute(json!({"query": "anything"})).await;
    assert!(!result.is_error(), "must not surface as a tool error");
    assert!(result.content().contains("unavailable"));
}

#[tokio::test]
async fn execute_rejects_malformed_args_recoverably() {
    let tool = RecallSessionTool::new("s1", "http://127.0.0.1:1", "p");
    let result = tool.execute(json!({"top_k": 3})).await; // missing 'query'
    assert!(result.is_error());
    assert!(!result.is_fatal());
}

#[tokio::test]
async fn execute_empty_when_no_session_tagged_results() {
    let results = vec![result_entry("belongs elsewhere", 0.9, &["session:other"])];
    let (base_url, _captured) = spawn_recall_mock(results).await;
    let tool = RecallSessionTool::new("s1", base_url, "p");

    let result = tool.execute(json!({"query": "anything"})).await;
    assert!(!result.is_error());
    assert!(result.content().contains("No session memory found"));
}
