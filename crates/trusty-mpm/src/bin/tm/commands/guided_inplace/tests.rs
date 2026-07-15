//! Unit tests for [`super`]'s in-place-relaunch client (#2023 C, #2789).
//!
//! Why: extracted from an inline `#[cfg(test)] mod tests` so the production
//! `guided_inplace.rs` stays under the 500-SLOC cap while these tests keep the
//! 1500-SLOC test budget (basename `tests.rs`).
//! What: the local `TcpListener` HTTP mocks and every `plan_inplace_*` /
//! `reactivate_*` / `fetch_until_stopped_*` case, moved verbatim.
//! Test: this file IS the test module.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::*;

const TEST_ID: &str = "11111111-2222-3333-4444-555555555555";

/// Spawn a background HTTP mock that replies `status_line` (+ empty JSON
/// body) to every connection, counting hits.
///
/// Why: `reactivate_managed_session`'s status-code handling (#2027) needs
/// a real HTTP round-trip to exercise reqwest's response parsing; this
/// mirrors `core::sm::providers::test_support`'s "read the full request
/// before replying" mock convention (that helper is `pub(crate)` to the
/// `trusty-mpm` LIB crate and unreachable from this `bin/tm` BINARY crate
/// — a separate compilation unit — hence the small inline copy in
/// [`read_full_request`] below, rather than pulling in an external
/// mock-server dependency for one test file).
/// What: binds an ephemeral port, loops accepting connections, and for
/// each one increments the returned counter, drains the request, then
/// replies. Runs until the test's tokio runtime shuts down.
/// Test: used by `reactivate_*` and the command-build-ordering test.
async fn spawn_mock(status_line: &'static str) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_task = hits.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            hits_task.fetch_add(1, Ordering::SeqCst);
            read_full_request(&mut sock).await;
            let body = "{}";
            let resp = format!(
                "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    (format!("http://{addr}"), hits)
}

/// Read an entire HTTP/1.1 request (headers + any `Content-Length` body)
/// before the caller writes a response — avoids the connection-reset
/// flakiness a naive single `read` can cause (mirrors
/// `core::sm::providers::test_support::read_full_request`, unreachable
/// from this crate; see [`spawn_mock`]'s doc for why it is duplicated
/// here rather than shared).
async fn read_full_request(sock: &mut TcpStream) {
    let mut buf: Vec<u8> = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
        match sock.read(&mut chunk).await {
            Ok(0) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => return,
        }
    };
    let content_len: usize = String::from_utf8_lossy(&buf[..header_end])
        .split("\r\n")
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())?
        })
        .unwrap_or(0);
    let want_total = header_end + content_len;
    while buf.len() < want_total {
        match sock.read(&mut chunk).await {
            Ok(0) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => return,
        }
    }
}

/// Spawn a background HTTP mock that captures the FIRST request line (the
/// `METHOD /path?query HTTP/1.1` line) of the first connection into the
/// returned shared string, then replies `status_line`.
///
/// Why: [`spawn_mock`] drains the request but discards it, so it cannot
/// prove which URL was requested. The #2789 wire contract — the caller's
/// pane id reaching the daemon as a query param — is only meaningful if the
/// exact request target is observable; this helper captures it.
/// What: binds an ephemeral port; for the first connection reads until the
/// end of the request headers, records the request line, then replies with
/// `status_line` + an empty JSON body. Subsequent connections are still
/// answered but not re-captured.
/// Test: `reactivate_forwards_caller_pane_id_query`,
/// `reactivate_omits_caller_pane_id_query_when_absent`.
async fn spawn_capturing_mock(status_line: &'static str) -> (String, Arc<Mutex<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let request_line = Arc::new(Mutex::new(String::new()));
    let request_line_task = request_line.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let mut buf: Vec<u8> = Vec::with_capacity(4096);
            let mut chunk = [0u8; 4096];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                match sock.read(&mut chunk).await {
                    Ok(0) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    Err(_) => break,
                }
            }
            if let Ok(mut slot) = request_line_task.lock()
                && slot.is_empty()
            {
                *slot = String::from_utf8_lossy(&buf)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string();
            }
            let body = "{}";
            let resp = format!(
                "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    (format!("http://{addr}"), request_line)
}

/// Spawn a background HTTP mock that replies 200 OK with `{"state": ..}`,
/// walking through `states` one entry per connection (clamping to the last
/// entry once exhausted) — used to simulate a record transitioning
/// `Active` -> `Stopped` across retries (#2148).
///
/// Why: [`fetch_managed_session_until_stopped`]'s retry behavior needs a
/// mock whose response changes across calls, unlike [`spawn_mock`]'s fixed
/// status line; this mirrors the same "read the full request before
/// replying" convention.
/// What: binds an ephemeral port, loops accepting connections, and for the
/// Nth connection replies with `states[min(N, states.len() - 1)]` as the
/// summary's `state` field. Runs until the test's tokio runtime shuts down.
/// Test: `fetch_until_stopped_*` below.
async fn spawn_state_mock(states: Vec<&'static str>) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let hits = Arc::new(AtomicUsize::new(0));
    let hits_task = hits.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let n = hits_task.fetch_add(1, Ordering::SeqCst);
            read_full_request(&mut sock).await;
            let idx = n.min(states.len().saturating_sub(1));
            let state = states.get(idx).copied().unwrap_or("active");
            let body = format!(r#"{{"id":"{TEST_ID}","name":"tm-test","state":"{state}"}}"#);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    (format!("http://{addr}"), hits)
}

#[tokio::test]
async fn fetch_until_stopped_returns_immediately_when_already_stopped() {
    // #2148: the common case — no race — must not pay any retry delay.
    let (url, hits) = spawn_state_mock(vec!["stopped"]).await;
    let client = reqwest::Client::new();
    let start = std::time::Instant::now();
    let record = fetch_managed_session_until_stopped(&client, &url, TEST_ID).await;
    assert_eq!(record.map(|r| r.state), Some("stopped".to_string()));
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "must not retry once the first fetch already reads stopped"
    );
    assert!(
        start.elapsed() < FETCH_RETRY_BUDGET,
        "must return promptly, not wait out the whole retry budget"
    );
}

#[tokio::test]
async fn fetch_until_stopped_retries_past_transitioning_state() {
    // #2148: the race this hardens against — the record briefly reads
    // "active" while the SessionEnd stop is still in flight, then settles
    // on "stopped" a couple of polls later.
    let (url, hits) = spawn_state_mock(vec!["active", "active", "stopped"]).await;
    let client = reqwest::Client::new();
    let record = fetch_managed_session_until_stopped(&client, &url, TEST_ID).await;
    assert_eq!(record.map(|r| r.state), Some("stopped".to_string()));
    assert!(
        hits.load(Ordering::SeqCst) >= 3,
        "must retry past the transitioning reads before accepting stopped"
    );
}

#[tokio::test]
async fn fetch_until_stopped_gives_up_after_budget_when_never_stopped() {
    // A genuinely non-stopped record (e.g. still active, or the id belongs
    // to some other running session) must not retry forever — it gives up
    // once the bounded budget elapses so the caller falls through promptly.
    let (url, hits) = spawn_state_mock(vec!["active"]).await;
    let client = reqwest::Client::new();
    let start = std::time::Instant::now();
    let record = fetch_managed_session_until_stopped(&client, &url, TEST_ID).await;
    let elapsed = start.elapsed();
    assert_eq!(record.map(|r| r.state), Some("active".to_string()));
    assert!(
        elapsed >= FETCH_RETRY_BUDGET,
        "must not give up before the retry budget elapses"
    );
    assert!(
        elapsed < FETCH_RETRY_BUDGET + FETCH_RETRY_INTERVAL * 3,
        "must not overrun the budget by more than a poll or two"
    );
    assert!(
        hits.load(Ordering::SeqCst) > 1,
        "must have retried at least once before giving up"
    );
}

#[test]
fn parse_show_environment_value_extracts_id() {
    assert_eq!(
        parse_show_environment_value(
            "TM_MANAGED_SESSION_ID=11111111-2222-3333-4444-555555555555\n"
        ),
        Some("11111111-2222-3333-4444-555555555555".to_string())
    );
}

#[test]
fn parse_show_environment_value_none_when_unset() {
    // tmux prints "-NAME" (no "=") when the variable is explicitly unset in
    // the session.
    assert_eq!(
        parse_show_environment_value("-TM_MANAGED_SESSION_ID\n"),
        None
    );
}

#[test]
fn parse_show_environment_value_none_when_empty() {
    assert_eq!(parse_show_environment_value(""), None);
    assert_eq!(
        parse_show_environment_value("TM_MANAGED_SESSION_ID=\n"),
        None
    );
}

#[test]
fn plan_inplace_selected_when_env_set_and_stopped() {
    assert_eq!(
        plan_inplace(Some(TEST_ID), Some("stopped")),
        Some(ResumeAction::InPlace)
    );
}

#[test]
fn plan_inplace_none_when_env_absent() {
    assert_eq!(plan_inplace(None, Some("stopped")), None);
    assert_eq!(plan_inplace(None, None), None);
}

#[test]
fn plan_inplace_none_when_env_set_but_unresolved() {
    // Guard case (#2023 C item 4): a stale/unknown id must fall through to
    // the ordinary guided picker rather than error.
    assert_eq!(plan_inplace(Some(TEST_ID), None), None);
}

#[test]
fn plan_inplace_none_when_resolved_but_not_stopped() {
    // Safety-boundary regression guard (#2027 code-critic WARN): a
    // resolved-but-non-Stopped record (Active/Errored/Decommissioned —
    // e.g. from a leaked/stale TM_MANAGED_SESSION_ID pointing at some
    // OTHER, currently-running session) must NOT select the in-place
    // path; it must fall through to the ordinary guided picker exactly
    // like an unresolved id.
    for state in ["active", "errored", "provisioning", "decommissioned"] {
        assert_eq!(
            plan_inplace(Some(TEST_ID), Some(state)),
            None,
            "state '{state}' must not select InPlace"
        );
    }
}

#[tokio::test]
async fn reactivate_confirms_success_on_2xx() {
    let (url, hits) = spawn_mock("HTTP/1.1 200 OK").await;
    let client = reqwest::Client::new();
    let ok = reactivate_managed_session(&client, &url, TEST_ID, None).await;
    assert!(ok, "a 2xx response must confirm reactivation");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn reactivate_aborts_on_409_conflict() {
    // #2027 HIGH fix: a 409 (session not Stopped on the daemon's own
    // re-check) must NOT be treated as confirmed — the caller must abort
    // rather than proceeding to exec.
    let (url, _hits) = spawn_mock("HTTP/1.1 409 Conflict").await;
    let client = reqwest::Client::new();
    let ok = reactivate_managed_session(&client, &url, TEST_ID, None).await;
    assert!(!ok, "409 must NOT be treated as a confirmed reactivate");
}

#[tokio::test]
async fn reactivate_aborts_on_404_not_found() {
    let (url, _hits) = spawn_mock("HTTP/1.1 404 Not Found").await;
    let client = reqwest::Client::new();
    let ok = reactivate_managed_session(&client, &url, TEST_ID, None).await;
    assert!(!ok, "404 must NOT be treated as a confirmed reactivate");
}

#[tokio::test]
async fn reactivate_aborts_on_unreachable_daemon() {
    // Nothing listens on a privileged low port -> immediate connection
    // refused, exercising the network-error branch without waiting out
    // the full PROBE_TIMEOUT.
    let client = reqwest::Client::new();
    let ok = reactivate_managed_session(&client, "http://127.0.0.1:1", TEST_ID, None).await;
    assert!(
        !ok,
        "an unreachable daemon must NOT be treated as a confirmed reactivate"
    );
}

#[tokio::test]
async fn reactivate_forwards_caller_pane_id_query() {
    // #2789: the caller's own tmux pane_id must reach the daemon as a
    // `?caller_pane_id=` query param — that is the whole mechanism by which
    // the daemon learns to treat the requesting `tm` pane as idle instead
    // of 409'ing. Capture the raw request line and assert the (URL-encoded)
    // pane id is present.
    let (url, request_line) = spawn_capturing_mock("HTTP/1.1 200 OK").await;
    let client = reqwest::Client::new();
    let ok = reactivate_managed_session(&client, &url, TEST_ID, Some("%3")).await;
    assert!(ok, "a 2xx response must confirm reactivation");
    let line = request_line.lock().expect("lock").clone();
    assert!(
        line.contains("caller_pane_id=%253"),
        "the request line must carry the URL-encoded caller_pane_id (%3 -> %253); got: {line}"
    );
}

#[tokio::test]
async fn reactivate_omits_caller_pane_id_query_when_absent() {
    // Backward compatibility: a non-tmux / pre-#2789 caller sends no
    // caller_pane_id, and the request must carry no such query param.
    let (url, request_line) = spawn_capturing_mock("HTTP/1.1 200 OK").await;
    let client = reqwest::Client::new();
    let ok = reactivate_managed_session(&client, &url, TEST_ID, None).await;
    assert!(ok);
    let line = request_line.lock().expect("lock").clone();
    assert!(
        !line.contains("caller_pane_id"),
        "no caller_pane_id query param may be sent when the caller passes None; got: {line}"
    );
}

#[tokio::test]
async fn run_inplace_relaunch_never_reactivates_when_command_build_fails() {
    // #2027 MEDIUM fix: command resolution (resolve `claude` + build the
    // resume argv) must happen BEFORE the daemon is ever asked to
    // reactivate the record, so a missing-binary failure never flips a
    // Stopped record to a false Active.
    //
    // This ordering guarantee can only be exercised end-to-end on a
    // machine where `claude` is NOT resolvable — otherwise
    // build_inplace_resume_command succeeds and this test would be
    // vacuously true. Probe first; skip (don't fail) when claude IS
    // present, mirroring the inverse of the "skip when claude absent"
    // convention used throughout runtime::claude_code's own test suite.
    let tmp = tempfile::tempdir().expect("tempdir");
    if trusty_mpm::runtime::build_inplace_resume_command(tmp.path(), None).is_ok() {
        return;
    }

    let (url, hits) = spawn_mock("HTTP/1.1 200 OK").await;
    let record = trusty_mpm::client::ManagedSessionSummary {
        id: TEST_ID.to_string(),
        name: "tm-test".to_string(),
        state: "stopped".to_string(),
        workspace_path: Some(tmp.path().to_string_lossy().to_string()),
        repo_url: None,
        branch: None,
        created_at: None,
        last_activity_at: None,
        pending_decision: None,
        proposed_default: None,
        source_id: None,
        task: None,
        cwd: None,
        claude_session_id: None,
        deliverable_id: None,
        pane_id: None,
        injection_status: None,
        unresumable: false,
    };
    let client = reqwest::Client::new();

    let outcome = run_inplace_relaunch(&client, &url, TEST_ID, record, None).await;

    assert!(
        matches!(outcome, InPlaceOutcome::Result(Err(_))),
        "a command-build failure must surface as a hard error, not FallThrough"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst),
        0,
        "reactivate must NEVER be called when command resolution fails first (#2027)"
    );
}
