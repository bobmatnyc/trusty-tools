//! `GET /api/costs` response-shape and state-distinction tests (#4098).
//!
//! Why: The route's whole value is that it tells four states apart — costs,
//! nothing recorded, nothing dispatched, and unreadable. Collapsing any pair of
//! them produces the confident-`$0.00`-over-a-missing-file failure the Costs
//! tab exists to avoid, and no other test would catch it.
//! What: `costs_at` exercised against tempdir fixtures for the payload shape and
//! each state, plus one router-level check that the path is actually wired.
//! Test: this file.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use super::test_router;
use crate::api::server::costs::costs_at;

/// Read an axum `Response` into a JSON value plus its status.
async fn read(resp: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .expect("body");
    let json = serde_json::from_slice(&bytes).expect("json body");
    (status, json)
}

/// Write `lines` as a project's usage log and return the tempdir.
fn log_with(lines: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = dir.path().join(".trusty-agents").join("state");
    std::fs::create_dir_all(&state).expect("mkdir");
    std::fs::write(state.join("usage.jsonl"), lines.join("\n")).expect("write");
    dir
}

fn row(ts: &str, agent: &str, model: &str, input: u32, output: u32) -> String {
    format!(
        r#"{{"ts":"{ts}","agent":"{agent}","model":"{model}","runner":"openrouter","input_tokens":{input},"output_tokens":{output},"duration_ms":10,"task_prefix":"t"}}"#
    )
}

/// Why (#4098): a route that exists in a module but not in the router is a
/// 404 the GUI cannot tell from a broken build.
/// What: `GET /api/costs` against the real router is not 404/405.
/// Test: this test.
#[tokio::test]
async fn costs_route_is_wired_into_router() {
    let app = test_router();
    let req = Request::builder()
        .uri("/api/costs")
        .body(Body::empty())
        .expect("request");
    let resp = app.oneshot(req).await.expect("response");
    assert_ne!(resp.status(), StatusCode::NOT_FOUND, "/api/costs unrouted");
    assert_ne!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
}

/// Why (#4098): the Costs tab binds directly to these field names, so the
/// payload shape is a contract, not an implementation detail.
/// What: Every documented field is present and the breakdowns carry the
/// documented row shape.
/// Test: this test.
#[tokio::test]
async fn costs_returns_totals_and_breakdowns() {
    let dir = log_with(&[
        &row(
            "2026-07-27T10:00:00Z",
            "assistant",
            "anthropic/claude-sonnet-4-6",
            1_000_000,
            0,
        ),
        &row(
            "2026-07-28T10:00:00Z",
            "ctrl",
            "anthropic/claude-haiku-4",
            1_000_000,
            0,
        ),
    ]);
    let (status, body) = read(costs_at(dir.path(), None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["available"], true);
    assert_eq!(body["records"], 2);
    assert_eq!(body["malformed_lines"], 0);
    assert!(body["source"].is_string());
    assert_eq!(body["first_ts"], "2026-07-27T10:00:00Z");
    assert_eq!(body["last_ts"], "2026-07-28T10:00:00Z");
    assert!(body["window_days"].is_null());

    // Sonnet $3/M + Haiku $0.80/M.
    let total = body["totals"]["cost_usd"].as_f64().expect("total cost");
    assert!((total - 3.80).abs() < 1e-6, "got {total}");
    assert_eq!(body["totals"]["key"], "total");
    assert_eq!(body["totals"]["dispatch_count"], 2);

    for group in ["by_agent", "by_model", "by_date"] {
        let rows = body[group].as_array().expect(group);
        assert_eq!(rows.len(), 2, "{group}");
        for r in rows {
            for field in [
                "key",
                "input_tokens",
                "output_tokens",
                "cost_usd",
                "dispatch_count",
                "duration_ms",
            ] {
                assert!(r.get(field).is_some(), "{group} row missing {field}");
            }
        }
    }
    // Descending by cost — Sonnet's agent leads.
    assert_eq!(body["by_agent"][0]["key"], "assistant");
    // Ascending by date.
    assert_eq!(body["by_date"][0]["key"], "2026-07-27");
}

/// Why (#4098): THE load-bearing behavior. A missing log must not render as
/// `$0.00` — the endpoint says so explicitly and the GUI repeats it.
/// What: A project with no state dir answers 200 with `available: false`, a
/// human-readable `reason`, and a zeroed-but-clearly-unavailable envelope.
/// Test: this test.
#[tokio::test]
async fn costs_reports_no_data_for_missing_log() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (status, body) = read(costs_at(dir.path(), None)).await;
    assert_eq!(status, StatusCode::OK, "absent data is not an HTTP error");
    assert_eq!(body["available"], false);
    assert!(
        body["reason"]
            .as_str()
            .expect("reason")
            .contains("no usage has been recorded"),
        "got {body}"
    );
    assert_eq!(body["records"], 0);
    assert_eq!(body["totals"]["cost_usd"], 0.0);
    assert!(body["by_agent"].as_array().expect("by_agent").is_empty());
    assert!(
        body["source"]
            .as_str()
            .expect("source")
            .contains("usage.jsonl")
    );
}

/// Why (#4098): a log that EXISTS but is empty is a different fact from one
/// that does not exist — the project has state and simply has not dispatched.
/// Conflating them would make "not set up" indistinguishable from "idle".
/// What: An empty file answers `available: true` with zero records.
/// Test: this test.
#[tokio::test]
async fn costs_distinguishes_empty_log_from_missing_log() {
    let dir = log_with(&[]);
    let (status, body) = read(costs_at(dir.path(), None)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["available"], true, "the log exists — it is just empty");
    assert_eq!(body["records"], 0);
    assert!(body.get("reason").is_none());
}

/// Why (#4098): totals over a partially-unreadable log are incomplete, and the
/// GUI can only warn about that if the count reaches it.
/// What: A log with two good rows and two bad ones reports both counts.
/// Test: this test.
#[tokio::test]
async fn costs_surfaces_malformed_line_count() {
    let dir = log_with(&[
        &row("2026-07-27T10:00:00Z", "a", "claude-haiku-4", 1_000_000, 0),
        "{ not json",
        r#"{"agent":"a","model":"m"}"#,
        &row("2026-07-27T11:00:00Z", "a", "claude-haiku-4", 1_000_000, 0),
    ]);
    let (status, body) = read(costs_at(dir.path(), None)).await;
    assert_eq!(status, StatusCode::OK, "partial data is still served");
    assert_eq!(body["records"], 2);
    assert_eq!(body["malformed_lines"], 2);
    let total = body["totals"]["cost_usd"].as_f64().expect("cost");
    assert!((total - 1.60).abs() < 1e-6, "got {total}");
}

/// Why (#4098): `?days=` is the only query knob the route implements, so its
/// effect on the payload is the contract the GUI's range control binds to.
/// What: `days=1` narrows a three-day log to its newest day and echoes the
/// window back; `days=0` is treated as "everything".
/// Test: this test.
#[tokio::test]
async fn costs_window_narrows_the_report() {
    let dir = log_with(&[
        &row("2026-07-26T10:00:00Z", "a", "claude-haiku-4", 1_000_000, 0),
        &row("2026-07-27T10:00:00Z", "a", "claude-haiku-4", 1_000_000, 0),
        &row("2026-07-28T10:00:00Z", "a", "claude-haiku-4", 1_000_000, 0),
    ]);
    let (_, all) = read(costs_at(dir.path(), None)).await;
    assert_eq!(all["records"], 3);

    let (status, one) = read(costs_at(dir.path(), Some(1))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(one["records"], 1);
    assert_eq!(one["window_days"], 1);
    assert_eq!(one["by_date"].as_array().expect("by_date").len(), 1);

    // `days=0` is meaningless as a window; treat it as unrestricted rather
    // than as "show nothing".
    let (_, zero) = read(costs_at(dir.path(), Some(0))).await;
    assert_eq!(zero["records"], 3);
    assert!(zero["window_days"].is_null());
}

/// Why (#4098): an unreadable-but-present log is a real fault (permissions, a
/// truncated mount). Degrading it to "no data" would hide a broken cost trail
/// behind an innocent message.
/// What: A `usage.jsonl` that is a DIRECTORY reads as an I/O error → 500.
/// Test: this test.
#[tokio::test]
async fn costs_reports_an_unreadable_log_as_a_server_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A directory where the file should be: exists, but never reads as text.
    std::fs::create_dir_all(dir.path().join(".trusty-agents/state/usage.jsonl")).expect("mkdir");
    let (status, body) = read(costs_at(dir.path(), None)).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["available"], false);
    assert!(
        body["error"]
            .as_str()
            .expect("error")
            .contains("could not read usage log"),
        "got {body}"
    );
}
