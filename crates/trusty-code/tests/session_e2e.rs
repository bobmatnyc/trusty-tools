//! API-driven end-to-end test for the #2054 session lifecycle.
//!
//! Why: the vision spec's Testability requirement (§9, "100% CLI/API
//! Testable" + "Per-Issue E2E Coverage") mandates that each issue's slice be
//! validated by spawning the REAL `tcode serve` daemon and driving it over
//! the actual wire protocol — never by calling into `trusty_code`'s Rust API
//! directly. This file is a genuine black-box driver: it spawns the
//! compiled `tcode` binary as a subprocess and speaks raw JSON-RPC 2.0 over
//! its real stdin/stdout (and, for the HTTP half, real TCP + HTTP).
//! What: [`session_lifecycle_over_stdio`] drives
//! create -> attach -> send -> receive a streamed event -> detach -> status
//! entirely over STDIO NDJSON. [`session_lifecycle_over_http_with_sse`]
//! drives the same lifecycle over `POST /rpc` plus the
//! `GET /sessions/{id}/events` SSE stream, proving the HTTP transport
//! observably streams too (per the ticket's "cover both transports" ask).
//! Test: this file IS the test; see `support` for the process/protocol
//! plumbing shared by both scenarios.

mod support;

use std::time::Duration;

use serde_json::{Value, json};
use support::{
    StdioSession, find_response, find_session_event, http_get_prefix, spawn_http_daemon,
};

/// create -> attach -> send -> receive a streamed event -> detach -> status,
/// entirely over `tcode serve --stdio`'s real stdin/stdout.
#[tokio::test]
async fn session_lifecycle_over_stdio() {
    let mut daemon = StdioSession::spawn();

    // 1. session.create
    let create_resp = daemon
        .call(1, "session.create", json!({"task": "e2e stdio task"}))
        .await;
    assert!(
        create_resp["error"].is_null(),
        "create failed: {create_resp}"
    );
    let session_id = create_resp["result"]["id"]
        .as_str()
        .expect("session.create must return an id")
        .to_string();
    assert_eq!(create_resp["result"]["status"], "running");

    // 2. session.attach — replay must include the creation-lifecycle events.
    let attach_resp = daemon
        .call(2, "session.attach", json!({"session_id": session_id}))
        .await;
    assert!(
        attach_resp["error"].is_null(),
        "attach failed: {attach_resp}"
    );
    let replay = attach_resp["result"]["events"]
        .as_array()
        .expect("attach must return a replay events array");
    assert!(
        replay.iter().any(|e| e["type"] == "session_started"),
        "replay must include session_started: {replay:?}"
    );
    assert_eq!(
        attach_resp["result"]["stream_url"],
        format!("/sessions/{session_id}/events")
    );

    // 3. session.send — must acknowledge AND (via the attach subscription)
    // push a live `session.event` notification, in either order on the wire.
    daemon
        .write_request(
            3,
            "session.send",
            json!({"session_id": session_id, "input": "hello"}),
        )
        .await;

    let mut got_ack = false;
    let mut got_event = false;
    let lines = daemon.read_lines(2).await;
    for line in &lines {
        if let Some(resp) = find_response(line, 3) {
            assert!(resp["error"].is_null(), "session.send failed: {resp}");
            assert_eq!(resp["result"]["acknowledged"], true);
            got_ack = true;
        }
        if let Some(event) = find_session_event(line, &session_id)
            && event["event"]["type"] == "session_input"
            && event["event"]["input"] == "hello"
        {
            got_event = true;
        }
    }
    assert!(
        got_ack,
        "never saw the session.send acknowledgement; lines: {lines:?}"
    );
    assert!(
        got_event,
        "never saw the streamed session_input event; lines: {lines:?}"
    );

    // 4. session.detach
    let detach_resp = daemon
        .call(4, "session.detach", json!({"session_id": session_id}))
        .await;
    assert!(
        detach_resp["error"].is_null(),
        "detach failed: {detach_resp}"
    );

    // 5. session.status — still running (detach doesn't cancel).
    let status_resp = daemon
        .call(5, "session.status", json!({"session_id": session_id}))
        .await;
    assert!(
        status_resp["error"].is_null(),
        "status failed: {status_resp}"
    );
    assert_eq!(status_resp["result"]["status"], "running");
    assert_eq!(status_resp["result"]["id"], session_id);

    daemon.shutdown_via_eof_and_assert_clean_exit().await;
}

/// The same lifecycle, driven over `POST /rpc` + the `GET
/// /sessions/{id}/events` SSE stream — proves the HTTP transport streams
/// observably too, not just STDIO.
#[tokio::test]
async fn session_lifecycle_over_http_with_sse() {
    let daemon = spawn_http_daemon().await;
    let client = reqwest::Client::new();
    let rpc_url = format!("{}/rpc", daemon.base_url);

    // 1. session.create
    let create_resp: Value = client
        .post(&rpc_url)
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "session.create", "params": {"task": "e2e http task"}}))
        .send()
        .await
        .expect("POST session.create")
        .json()
        .await
        .expect("parse session.create response");
    assert!(
        create_resp["error"].is_null(),
        "create failed: {create_resp}"
    );
    let session_id = create_resp["result"]["id"].as_str().unwrap().to_string();

    // 2. GET /sessions/{id}/events — read a bounded prefix; the replay burst
    // must include the creation events.
    let events_url = format!("{}/sessions/{session_id}/events", daemon.base_url);
    let replay_text = http_get_prefix(
        &client,
        &events_url,
        "session_status_changed",
        Duration::from_secs(5),
    )
    .await;
    assert!(
        replay_text.contains("session_started"),
        "replay body: {replay_text}"
    );

    // 3. session.send over POST /rpc.
    let send_resp: Value = client
        .post(&rpc_url)
        .json(&json!({"jsonrpc": "2.0", "id": 2, "method": "session.send", "params": {"session_id": session_id, "input": "hello-http"}}))
        .send()
        .await
        .expect("POST session.send")
        .json()
        .await
        .expect("parse session.send response");
    assert!(send_resp["error"].is_null(), "send failed: {send_resp}");
    assert_eq!(send_resp["result"]["acknowledged"], true);

    // 4. Continue reading the SAME SSE endpoint (a fresh GET is fine — every
    // GET gets its own live subscription, and the ring buffer already holds
    // the input event by the time this request lands) for the live event.
    let live_text =
        http_get_prefix(&client, &events_url, "hello-http", Duration::from_secs(5)).await;
    assert!(
        live_text.contains("session_input"),
        "live body: {live_text}"
    );

    // 5. session.cancel over POST /rpc.
    let cancel_resp: Value = client
        .post(&rpc_url)
        .json(&json!({"jsonrpc": "2.0", "id": 3, "method": "session.cancel", "params": {"session_id": session_id}}))
        .send()
        .await
        .expect("POST session.cancel")
        .json()
        .await
        .expect("parse session.cancel response");
    assert!(
        cancel_resp["error"].is_null(),
        "cancel failed: {cancel_resp}"
    );
    assert_eq!(cancel_resp["result"]["status"], "cancelled");

    daemon.shutdown_via_sigterm_and_assert_clean_exit().await;
}
