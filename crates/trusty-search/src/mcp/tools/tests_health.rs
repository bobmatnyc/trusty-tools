//! `search_health` structured-diagnostics contract tests (#5264).
//!
//! Why: the arm used to forward `GET /health` verbatim, so all three states a
//! caller must act on differently arrived the same way — a connection refusal
//! and an HTTP 500 both became one `DispatchError::Transport` prose string, and
//! a healthy 200 named neither the responder nor whether this project was
//! indexed on it. Each test below fails against the pre-fix arm because the
//! report fields it asserts on did not exist.
//! What: drives `McpServer::dispatch` against loopback daemons fixed at each
//! state, and reads the report back out of the `tools/call` envelope.
//! Test: this file.

use serde_json::{json, Value};

use super::tests::req;
use super::{
    McpServer, HEALTH_DAEMON_ERROR, HEALTH_DAEMON_UNREACHABLE, HEALTH_INDEX_EMPTY,
    HEALTH_INDEX_NOT_REGISTERED, HEALTH_INDEX_UNKNOWN, HEALTH_OK,
};

/// A loopback daemon whose `/health` and `/indexes/{id}/status` responses are
/// both fixed by the caller.
///
/// Why: the three verdicts are defined by the HTTP status on two different
/// routes, so a mock that can only fix one of them cannot express them.
/// What: returns the base URL. `health` and `status` are each `(code, body)`;
/// a `body` of [`Value::Null`] is served as a plain-text body so the
/// "answered, but not with a health object" path is reachable.
/// Test: used by every test in this file.
async fn spawn_health_daemon(health: (u16, Value), status: (u16, Value)) -> String {
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::response::{IntoResponse, Response as AxumResponse};
    use axum::routing::get;
    use axum::Router;

    #[derive(Clone)]
    struct S {
        health: (StatusCode, Value),
        status: (StatusCode, Value),
    }

    fn render(code: StatusCode, body: &Value) -> AxumResponse {
        match body {
            Value::Null => (code, "not json at all").into_response(),
            v => (code, axum::Json(v.clone())).into_response(),
        }
    }

    async fn health_handler(State(s): State<S>) -> AxumResponse {
        render(s.health.0, &s.health.1)
    }
    async fn status_handler(State(s): State<S>) -> AxumResponse {
        render(s.status.0, &s.status.1)
    }

    let state = S {
        health: (StatusCode::from_u16(health.0).expect("status"), health.1),
        status: (StatusCode::from_u16(status.0).expect("status"), status.1),
    };
    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/indexes/{id}/status", get(status_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

/// A healthy `/health` body shaped like the daemon's real one.
fn healthy_body(indexes: u64, total_chunks: u64) -> Value {
    json!({
        "status": "ok",
        "version": "9.9.9",
        "indexes": indexes,
        "total_chunks": total_chunks,
        "uptime_secs": 12,
        "embedder": "ready",
    })
}

/// Call `search_health` through `tools/call` and return the parsed report.
async fn health_report(server: &McpServer, args: Value) -> Value {
    let resp = server
        .dispatch(req(
            "tools/call",
            json!({ "name": "search_health", "arguments": args }),
        ))
        .await;
    let result = resp.result.expect("tools/call always returns a result");
    let text = result["content"][0]["text"]
        .as_str()
        .expect("a text content node");
    serde_json::from_str(text).unwrap_or_else(|e| panic!("report is JSON: {e}\n{text}"))
}

/// Nothing listening is its own verdict, with the command that fixes it.
///
/// Pre-fix this arm returned `Err(Transport)`, so the response was
/// `isError: true` carrying a reqwest string — `result["status"]` did not
/// exist and this test failed at the `serde_json::from_str` of a non-JSON
/// `Error: GET …` body.
#[tokio::test(flavor = "multi_thread")]
async fn search_health_reports_daemon_unreachable_with_remediation() {
    // Port 1 is reserved and unbound on every developer machine.
    let server = McpServer::new("http://127.0.0.1:1");

    let report = health_report(&server, json!({})).await;

    assert_eq!(report["status"], HEALTH_DAEMON_UNREACHABLE);
    assert_eq!(report["healthy"], Value::Bool(false));
    assert_eq!(report["daemon"]["reachable"], Value::Bool(false));
    assert_eq!(report["daemon"]["base_url"], "http://127.0.0.1:1");
    let remediation = report["remediation"].as_str().expect("remediation");
    assert!(
        remediation.contains("trusty-search start"),
        "remediation must name the command that fixes it: {remediation}"
    );
}

/// A daemon that answers badly is NOT the same verdict as one that is absent.
#[tokio::test(flavor = "multi_thread")]
async fn search_health_reports_a_non_2xx_daemon() {
    let base = spawn_health_daemon(
        (500, json!({ "error": "boom" })),
        (200, json!({ "chunk_count": 1 })),
    )
    .await;
    let server = McpServer::new(base.clone());

    let report = health_report(&server, json!({ "index_id": "any" })).await;

    assert_eq!(report["status"], HEALTH_DAEMON_ERROR);
    assert_eq!(report["healthy"], Value::Bool(false));
    assert_eq!(report["daemon"]["reachable"], Value::Bool(true));
    assert_eq!(report["daemon"]["http_status"], 500);
    assert_eq!(report["daemon"]["base_url"], base);
}

/// A 2xx from something that is not this daemon must not read as healthy.
#[tokio::test(flavor = "multi_thread")]
async fn search_health_rejects_a_2xx_body_that_is_not_a_health_object() {
    let base = spawn_health_daemon((200, Value::Null), (404, json!({}))).await;
    let server = McpServer::new(base);

    let report = health_report(&server, json!({ "index_id": "any" })).await;

    assert_eq!(report["status"], HEALTH_DAEMON_ERROR);
    assert_eq!(report["healthy"], Value::Bool(false));
    let body = report["daemon"]["body"].as_str().unwrap_or_default();
    assert!(
        body.contains("not a JSON health object"),
        "the report must say what was wrong with the body: {body}"
    );
}

/// A healthy daemon with no index for this project is the third state — and
/// the remediation is to index, not to start anything.
#[tokio::test(flavor = "multi_thread")]
async fn search_health_reports_an_unregistered_project_index() {
    let base = spawn_health_daemon(
        (200, healthy_body(42, 430_000)),
        (404, json!({ "error": "unknown index: mine" })),
    )
    .await;
    let server = McpServer::new(base).with_pinned_index("mine");

    let report = health_report(&server, json!({})).await;

    assert_eq!(report["status"], HEALTH_INDEX_NOT_REGISTERED);
    assert_eq!(report["healthy"], Value::Bool(false));
    assert_eq!(report["index"]["index_id"], "mine");
    assert_eq!(report["index"]["registered"], Value::Bool(false));
    assert_eq!(report["index"]["resolved_from"], "session_pin");
    let remediation = report["remediation"].as_str().expect("remediation");
    assert!(
        remediation.contains("trusty-search index"),
        "an unindexed project is fixed by indexing it: {remediation}"
    );
}

/// A registered but empty index is distinct from an absent one: searches
/// against it return nothing, and the report must say so.
#[tokio::test(flavor = "multi_thread")]
async fn search_health_reports_a_registered_but_empty_index() {
    let base = spawn_health_daemon(
        (200, healthy_body(1, 0)),
        (
            200,
            json!({ "index_id": "mine", "chunk_count": 0, "root_path": "/x" }),
        ),
    )
    .await;
    let server = McpServer::new(base).with_pinned_index("mine");

    let report = health_report(&server, json!({})).await;

    assert_eq!(report["status"], HEALTH_INDEX_EMPTY);
    assert_eq!(report["healthy"], Value::Bool(false));
    assert_eq!(report["index"]["chunk_count"], 0);
}

/// The whole point of #5264's health rewrite: a caller can tell WHICH daemon
/// answered, so "healthy" and "healthy, but not yours" are distinguishable.
///
/// The counts here are the ones from the live reproduction — an isolated
/// project silently attached to the machine's production daemon and got a
/// green 200 back that said nothing about it.
#[tokio::test(flavor = "multi_thread")]
async fn search_health_names_the_answering_daemon() {
    let base = spawn_health_daemon(
        (200, healthy_body(42, 430_000)),
        (200, json!({ "index_id": "mine", "chunk_count": 7 })),
    )
    .await;
    let server = McpServer::new(base.clone()).with_pinned_index("mine");

    let report = health_report(&server, json!({})).await;

    assert_eq!(report["status"], HEALTH_OK);
    assert_eq!(report["healthy"], Value::Bool(true));
    assert_eq!(report["daemon"]["base_url"], base);
    assert_eq!(report["daemon"]["version"], "9.9.9");
    assert_eq!(report["daemon"]["indexes"], 42);
    assert_eq!(report["daemon"]["total_chunks"], 430_000);

    let message = report["message"].as_str().expect("message");
    assert!(
        message.contains(&base) && message.contains("42") && message.contains("430000"),
        "the prose must identify the responder too: {message}"
    );
}

/// A daemon that is up tells you nothing about a project that was never
/// checked, so the unresolvable-scope branch must not report `ok`.
///
/// Why: `resolve_scope` returns `None` when there is no explicit `index_id`, no
/// session pin, and no id derivable from the working directory. Returning
/// `HEALTH_OK` there was a green verdict on a check that never ran — the same
/// unverified-green this tool exists to stop, one layer down from the daemon.
/// What: calls the injectable core with `scope: None` against a demonstrably
/// healthy daemon, and asserts the verdict is neither `ok` nor `healthy`.
/// Test: this test.
#[tokio::test(flavor = "multi_thread")]
async fn search_health_does_not_report_ok_when_no_index_could_be_resolved() {
    let base = spawn_health_daemon(
        (200, healthy_body(42, 430_000)),
        (200, json!({ "chunk_count": 1 })),
    )
    .await;
    let server = McpServer::new(base);

    let report = super::health::report_health(&server, None).await;

    assert_eq!(report["status"], HEALTH_INDEX_UNKNOWN);
    assert_eq!(
        report["healthy"],
        Value::Bool(false),
        "a project that was never checked must not read as healthy: {report}"
    );
    assert_eq!(report["index"], Value::Null);
    // The daemon half still reports truthfully — it WAS reached.
    assert_eq!(report["daemon"]["reachable"], Value::Bool(true));
    let message = report["message"].as_str().expect("message");
    assert!(
        message.contains("NO project-level check ran"),
        "the message must say the project was not checked: {message}"
    );
}

/// An unreadable chunk count is not a count of zero.
///
/// Why (#5633): `status.rs` deliberately reports `chunk_count: null` under HTTP
/// 200 when the durable corpus failed to open — #4333 chose `null` over the
/// in-memory fallback precisely because that fallback reported 122 for an index
/// holding 201,206 chunks. Reading that `null` as `0` turned the daemon's "I do
/// not know" into this tool's "it holds nothing", and sent the caller to
/// `trusty-search index` — a reindex, which is the WRONG action against a
/// write-quarantined corpus.
/// What: serves a 200 status body with `chunk_count: null` plus the
/// `corpus_open_failure` block the daemon sends alongside it, and asserts the
/// verdict is `index_unknown` rather than `index_empty`, with a remediation that
/// does not tell the caller to reindex.
/// Test: this IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn search_health_does_not_report_an_unreadable_chunk_count_as_empty() {
    let base = spawn_health_daemon(
        (200, healthy_body(1, 0)),
        (
            200,
            json!({
                "index_id": "mine",
                "chunk_count": Value::Null,
                "root_path": "/x",
                "corpus_open_failure": {
                    "kind": "open_timeout",
                    "transient": true,
                    "reason": "redb open timed out after 30s",
                },
            }),
        ),
    )
    .await;
    let server = McpServer::new(base).with_pinned_index("mine");

    let report = health_report(&server, json!({})).await;

    assert_eq!(
        report["status"], HEALTH_INDEX_UNKNOWN,
        "an unreadable count must not be rendered as an empty index: {report}"
    );
    assert_eq!(report["healthy"], Value::Bool(false));
    assert_eq!(
        report["index"]["chunk_count"],
        Value::Null,
        "the daemon's own null must pass through, not become 0: {report}"
    );

    let remediation = report["remediation"].as_str().expect("remediation");
    assert!(
        !remediation.contains("trusty-search index") && !remediation.contains("doctor --fix"),
        "reindexing a write-quarantined corpus is the wrong action: {remediation}"
    );
    assert!(
        remediation.contains("Do NOT reindex"),
        "an unknown count must actively steer the caller away from the reindex \
         the `index_empty` verdict used to prescribe: {remediation}"
    );

    // Report WHY, not just the value: the corpus failure the daemon already
    // sent must reach the caller rather than being dropped.
    let message = report["message"].as_str().expect("message");
    assert!(
        message.contains("corpus"),
        "the message must say the count was unreadable because the corpus \
         would not open: {message}"
    );
    assert_eq!(
        report["index"]["corpus_open_failure"]["kind"],
        "open_timeout"
    );
}

/// A 200 status body that simply omits `chunk_count` is also unknown, not zero.
///
/// Why (#5633): the same `unwrap_or(0)` swallowed an absent key exactly as it
/// swallowed an explicit `null`. Both mean the count was never established, and
/// neither is evidence of an empty index.
/// What: serves a 200 body with no `chunk_count` key at all and asserts the
/// verdict is `index_unknown`.
/// Test: this IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn search_health_does_not_report_a_missing_chunk_count_as_empty() {
    let base = spawn_health_daemon(
        (200, healthy_body(1, 0)),
        (200, json!({ "index_id": "mine", "root_path": "/x" })),
    )
    .await;
    let server = McpServer::new(base).with_pinned_index("mine");

    let report = health_report(&server, json!({})).await;

    assert_eq!(
        report["status"], HEALTH_INDEX_UNKNOWN,
        "an absent chunk_count is not a count of zero: {report}"
    );
    assert_eq!(report["healthy"], Value::Bool(false));
}

/// An explicit `index_id` argument outranks the session pin, and the report
/// says which source decided.
#[tokio::test(flavor = "multi_thread")]
async fn search_health_reports_which_source_named_the_index() {
    let base = spawn_health_daemon(
        (200, healthy_body(2, 10)),
        (200, json!({ "chunk_count": 3 })),
    )
    .await;
    let server = McpServer::new(base).with_pinned_index("pinned-one");

    let report = health_report(&server, json!({ "index_id": "explicit-one" })).await;

    assert_eq!(report["index"]["index_id"], "explicit-one");
    assert_eq!(report["index"]["resolved_from"], "argument");
}
