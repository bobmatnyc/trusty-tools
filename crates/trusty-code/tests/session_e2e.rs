//! API-driven end-to-end test for the #2054 session lifecycle, extended by
//! #2055 to assert on the formalised event taxonomy + envelope over the
//! real wire.
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
//! -> re-attach -> cancel entirely over STDIO NDJSON, asserting (#2055)
//! that every envelope carries `session_id`/`seq`/`at`/`kind`/`event`, that
//! every kind in the lifecycle taxonomy appears, and that `seq` is
//! gap-free and strictly increasing across the WHOLE session (replay ->
//! live -> re-attach's replay -> live again). [`session_lifecycle_over_http_with_sse`]
//! drives the same lifecycle over `POST /rpc` plus a SINGLE
//! `GET /sessions/{id}/events` SSE connection read in two stages (replay,
//! then a live event on the SAME connection), proving replay -> live
//! continuity over HTTP too.
//! Test: this file IS the test; see `support` for the process/protocol
//! plumbing shared by both scenarios.

mod support;

use std::time::Duration;

use serde_json::json;
use support::{
    StdioSession, assert_envelopes_contiguous, find_response, find_session_event, open_sse,
    parse_sse_frames, read_sse_until, spawn_http_daemon,
};

/// create -> attach -> send -> receive a streamed event -> detach -> status
/// -> re-attach -> cancel, entirely over `tcode serve --stdio`'s real
/// stdin/stdout. Asserts the #2055 envelope + taxonomy + seq-continuity
/// requirements throughout.
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

    // 2. session.attach — replay must be envelope-shaped and gap-free from 1.
    let attach_resp = daemon
        .call(2, "session.attach", json!({"session_id": session_id}))
        .await;
    assert!(
        attach_resp["error"].is_null(),
        "attach failed: {attach_resp}"
    );
    let replay = attach_resp["result"]["events"]
        .as_array()
        .expect("attach must return a replay events array")
        .clone();
    let (first_seq, last_seq) = assert_envelopes_contiguous(&replay);
    assert_eq!(first_seq, 1);
    assert_eq!(last_seq, 2);
    let kinds: Vec<&str> = replay.iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, vec!["session_started", "session_status_changed"]);
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
    let mut input_envelope = None;
    for line in daemon.read_lines(2).await {
        if let Some(resp) = find_response(&line, 3) {
            assert!(resp["error"].is_null(), "session.send failed: {resp}");
            assert_eq!(resp["result"]["acknowledged"], true);
            got_ack = true;
        }
        if let Some(envelope) = find_session_event(&line, &session_id) {
            input_envelope = Some(envelope);
        }
    }
    assert!(got_ack, "never saw the session.send acknowledgement");
    let input_envelope = input_envelope.expect("never saw the streamed session_input event");
    assert_envelopes_contiguous(std::slice::from_ref(&input_envelope));
    assert_eq!(input_envelope["seq"], last_seq + 1);
    assert_eq!(input_envelope["kind"], "session_input");
    assert_eq!(input_envelope["event"]["input"], "hello");

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

    // 6. Re-attach (same STDIO connection): its OWN replay burst must be
    // gap-free from 1 through the input event recorded in step 3 (seq 3).
    let reattach_resp = daemon
        .call(6, "session.attach", json!({"session_id": session_id}))
        .await;
    assert!(
        reattach_resp["error"].is_null(),
        "re-attach failed: {reattach_resp}"
    );
    let reattach_replay = reattach_resp["result"]["events"]
        .as_array()
        .unwrap()
        .clone();
    let (re_first, re_last) = assert_envelopes_contiguous(&reattach_replay);
    assert_eq!(re_first, 1);
    assert_eq!(re_last, 3);

    // 7. session.cancel — must publish 3 more events (status_changed,
    // session_cancelled, session_done), forwarded live via the re-attach.
    daemon
        .write_request(7, "session.cancel", json!({"session_id": session_id}))
        .await;
    let mut cancel_events = Vec::new();
    let mut got_cancel_ack = false;
    for line in daemon.read_lines(4).await {
        if let Some(resp) = find_response(&line, 7) {
            assert!(resp["error"].is_null(), "session.cancel failed: {resp}");
            assert_eq!(resp["result"]["status"], "cancelled");
            got_cancel_ack = true;
        }
        if let Some(envelope) = find_session_event(&line, &session_id) {
            cancel_events.push(envelope);
        }
    }
    assert!(
        got_cancel_ack,
        "never saw the session.cancel acknowledgement"
    );
    assert_eq!(
        cancel_events.len(),
        3,
        "expected status_changed + session_cancelled + session_done: {cancel_events:?}"
    );
    let (cancel_first, cancel_last) = assert_envelopes_contiguous(&cancel_events);
    assert_eq!(
        cancel_first,
        re_last + 1,
        "cancel events must continue the seq with no gap after re-attach's replay"
    );
    assert_eq!(cancel_last, re_last + 3);
    let cancel_kinds: Vec<&str> = cancel_events
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        cancel_kinds,
        vec![
            "session_status_changed",
            "session_cancelled",
            "session_done"
        ]
    );

    // The FULL session history (replay + input + re-attach replay's tail is
    // redundant with the first replay, so just check replay ++ input ++
    // cancel_events) must itself be one gap-free sequence from 1.
    let mut full_history = replay;
    full_history.push(input_envelope);
    full_history.extend(cancel_events);
    let (full_first, full_last) = assert_envelopes_contiguous(&full_history);
    assert_eq!(full_first, 1);
    assert_eq!(full_last, 6);

    daemon.shutdown_via_eof_and_assert_clean_exit().await;
}

/// The same lifecycle, driven over `POST /rpc` + a SINGLE
/// `GET /sessions/{id}/events` SSE connection read in two stages — proves
/// replay -> live seq continuity over HTTP too, not just STDIO.
#[tokio::test]
async fn session_lifecycle_over_http_with_sse() {
    let daemon = spawn_http_daemon().await;
    let client = reqwest::Client::new();
    let rpc_url = format!("{}/rpc", daemon.base_url);

    // 1. session.create
    let create_resp: serde_json::Value = client
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

    // 2. Open ONE SSE connection and read the replay burst from it.
    let events_url = format!("{}/sessions/{session_id}/events", daemon.base_url);
    let mut sse = open_sse(&client, &events_url).await;
    let mut buffer = Vec::new();
    read_sse_until(
        &mut sse,
        &mut buffer,
        "session_status_changed",
        Duration::from_secs(5),
    )
    .await;
    let replay_frames = parse_sse_frames(&String::from_utf8_lossy(&buffer));
    let (first_seq, last_seq) = assert_envelopes_contiguous(&replay_frames);
    assert_eq!(first_seq, 1);
    assert_eq!(last_seq, 2);
    let kinds: Vec<&str> = replay_frames
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["session_started", "session_status_changed"]);

    // 3. session.send over POST /rpc, while the SAME SSE connection is open.
    let send_resp: serde_json::Value = client
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

    // 4. Continue reading the SAME SSE connection for the live event —
    // this is the replay -> live continuity proof over HTTP.
    read_sse_until(&mut sse, &mut buffer, "hello-http", Duration::from_secs(5)).await;
    let all_frames = parse_sse_frames(&String::from_utf8_lossy(&buffer));
    let (full_first, full_last) = assert_envelopes_contiguous(&all_frames);
    assert_eq!(full_first, 1);
    assert_eq!(
        full_last, 3,
        "the live event must continue the replay's seq with no gap"
    );
    assert_eq!(all_frames.last().unwrap()["kind"], "session_input");
    assert_eq!(all_frames.last().unwrap()["event"]["input"], "hello-http");

    // 5. session.cancel over POST /rpc.
    let cancel_resp: serde_json::Value = client
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

    // 6. The SAME SSE connection must observe the cancel-triggered events
    // too, continuing the seq with no gap.
    read_sse_until(
        &mut sse,
        &mut buffer,
        "session_done",
        Duration::from_secs(5),
    )
    .await;
    let final_frames = parse_sse_frames(&String::from_utf8_lossy(&buffer));
    let (_, cancel_last) = assert_envelopes_contiguous(&final_frames);
    assert_eq!(cancel_last, 6);
    let final_kinds: Vec<&str> = final_frames
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        final_kinds,
        vec![
            "session_started",
            "session_status_changed",
            "session_input",
            "session_status_changed",
            "session_cancelled",
            "session_done",
        ]
    );

    // Graceful shutdown drains in-flight requests before exiting (issue
    // #534); an open SSE connection never "completes" on its own, so the
    // client must close it first or the daemon would wait for it forever.
    drop(sse);
    daemon.shutdown_via_sigterm_and_assert_clean_exit().await;
}
