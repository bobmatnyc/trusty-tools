//! Unit tests for `commands::rename`'s pane-identity cross-check (issue #3600).
//!
//! Why: split from an inline `mod tests` (mirroring `managed.rs`/
//! `managed_tests.rs`, `prune.rs`/`prune_tests.rs`) to keep `rename.rs` well
//! under the 500-SLOC production cap.
//! What: `confirm_rename_target_pane_*` cover the pure decision directly
//! (no I/O); `resolve_in_session_rename_target_*` cover the async fetch +
//! decision composition against a one-shot mock daemon (mirrors
//! `pm_guard_deny_by_default.rs`'s `spawn_mock_daemon` pattern), proving:
//! (1) causality — a genuinely DIFFERENT pane than the session's own record
//! is refused, where pre-fix code would have proceeded unconditionally on the
//! bare env var; (2) the legitimate path — a matching pane still resolves
//! normally.
//! Test: this file IS the test.

use super::*;

// ── confirm_rename_target_pane (pure decision) ───────────────────────────────

#[test]
fn confirm_rename_target_pane_ok_when_confirmed() {
    // The legitimate path: this pane's own pane_id matches the session's own
    // captured pane_id — must proceed.
    assert!(confirm_rename_target_pane("sess-1", Some("%5"), Some("%5")).is_ok());
}

#[test]
fn confirm_rename_target_pane_refuses_on_mismatch() {
    // The #3600 hijack: a sibling pane inherited the SAME
    // `TM_MANAGED_SESSION_ID` via tmux's session-scoped env propagation, but
    // its own pane_id ("%9") differs from the session's captured pane_id
    // ("%5") — must refuse, naming both pane ids and the session id.
    let err = confirm_rename_target_pane("sess-1", Some("%9"), Some("%5"))
        .expect_err("mismatched pane ids must refuse");
    assert!(err.contains("sess-1"), "must name the session id: {err}");
    assert!(err.contains("%9"), "must name this pane's id: {err}");
    assert!(
        err.contains("%5"),
        "must name the session's own pane id: {err}"
    );
}

#[test]
fn confirm_rename_target_pane_refuses_when_unavailable() {
    // Not inside tmux (or the tmux query failed) — identity cannot be
    // verified at all. Must refuse, NOT silently trust the env var (the
    // exact regression this issue exists to prevent).
    let err = confirm_rename_target_pane("sess-1", None, Some("%5"))
        .expect_err("unresolvable current pane id must refuse");
    assert!(err.contains("sess-1"));

    // A legacy/unknown record with no captured pane_id — same treatment.
    let err = confirm_rename_target_pane("sess-1", Some("%5"), None)
        .expect_err("unresolvable record pane id must refuse");
    assert!(err.contains("sess-1"));
}

// ── resolve_in_session_rename_target (async fetch + decision) ───────────────

/// Spawn a one-shot HTTP mock daemon that replies to ANY request (the
/// `GET /api/v1/sessions/managed` list call `resolve_managed_summary` makes)
/// with a canned `{"sessions": [...]}` body, mirroring
/// `pm_guard_deny_by_default.rs`'s `spawn_mock_daemon`.
///
/// Test: used by `resolve_in_session_rename_target_confirmed`,
/// `resolve_in_session_rename_target_refuses_on_pane_mismatch`,
/// `resolve_in_session_rename_target_refuses_when_record_not_found`.
async fn spawn_mock_managed_list_daemon(sessions_json_array: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let body = format!(r#"{{"sessions":{sessions_json_array}}}"#);
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.expect("accept");
        let mut buf = [0u8; 1024];
        let _ = sock.read(&mut buf).await;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = sock.write_all(resp.as_bytes()).await;
        let _ = sock.shutdown().await;
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn resolve_in_session_rename_target_confirmed() {
    // The legitimate path: the daemon's record for "sess-1" carries pane_id
    // "%5", and this process's OWN current pane is also "%5" — must resolve
    // (proving the healthy path is not broken by the new guard).
    let url = spawn_mock_managed_list_daemon(
        r#"[{"id":"sess-1","name":"tm-test","state":"active","pane_id":"%5"}]"#,
    )
    .await;
    let client = reqwest::Client::new();
    let record = resolve_in_session_rename_target(&client, &url, "sess-1", Some("%5"))
        .await
        .expect("matching pane id must resolve");
    assert_eq!(record.id, "sess-1");
}

#[tokio::test]
async fn resolve_in_session_rename_target_refuses_on_pane_mismatch() {
    // Causality proof: against PRE-FIX code (a bare `std::env::var` read with
    // no cross-check), this exact scenario — a sibling pane holding the same
    // inherited `TM_MANAGED_SESSION_ID` — would proceed to rename "sess-1"
    // unconditionally. With the fix, the record's own pane_id ("%5") does NOT
    // match this process's current pane ("%9"), so it must refuse.
    let url = spawn_mock_managed_list_daemon(
        r#"[{"id":"sess-1","name":"tm-test","state":"active","pane_id":"%5"}]"#,
    )
    .await;
    let client = reqwest::Client::new();
    let err = resolve_in_session_rename_target(&client, &url, "sess-1", Some("%9"))
        .await
        .expect_err("mismatched pane must refuse, not rename the wrong session");
    assert!(err.contains("sess-1"));
    assert!(err.contains("%9"));
    assert!(err.contains("%5"));
}

#[tokio::test]
async fn resolve_in_session_rename_target_refuses_when_record_not_found() {
    // `session_id` is unknown to the daemon (e.g. a stale/deleted session) —
    // identity cannot be proven either, so this must also refuse rather than
    // fall through to a bare rename attempt.
    let url = spawn_mock_managed_list_daemon("[]").await;
    let client = reqwest::Client::new();
    let err = resolve_in_session_rename_target(&client, &url, "sess-1", Some("%5"))
        .await
        .expect_err("an unresolvable record must refuse");
    assert!(err.contains("sess-1"));
}

// ── server-confirmed rename name (#3692 review HIGH-1) ──────────────────────

#[test]
fn rename_success_message_plain() {
    // The daemon confirmed exactly the requested name — plain success line.
    let msg = rename_success_message("sess-1", "tm-new", Some("tm-new"));
    assert_eq!(msg, "renamed sess-1 -> tm-new");
}

#[test]
fn rename_success_message_notes_suffix_on_collision() {
    // The daemon auto-suffixed a colliding name (#3692): the message must
    // carry the ACTUAL name and say the requested one was taken — pre-fix
    // code printed the requested name and the operator's next
    // attach/resume-by-name would target a session that doesn't exist.
    let msg = rename_success_message("sess-1", "tm-new", Some("tm-new-2"));
    assert!(
        msg.contains("tm-new-2"),
        "must print the applied name: {msg}"
    );
    assert!(
        msg.contains("was taken") && msg.contains("auto-suffixed"),
        "must explain the suffix: {msg}"
    );
}

#[test]
fn rename_success_message_falls_back_without_body() {
    // An older daemon whose response body carries no parseable `name` —
    // fall back to the requested name rather than printing nothing.
    let msg = rename_success_message("sess-1", "tm-new", None);
    assert_eq!(msg, "renamed sess-1 -> tm-new");
}

/// Spawn a one-shot mock daemon that answers the rename PATCH with `status`
/// and `body` — lets `do_rename_request` be tested end-to-end (HTTP → parse →
/// message) without a real daemon.
async fn spawn_mock_rename_daemon(status: &str, body: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.expect("accept");
        let mut buf = [0u8; 2048];
        let _ = sock.read(&mut buf).await;
        let _ = sock.write_all(resp.as_bytes()).await;
        let _ = sock.shutdown().await;
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn do_rename_request_reports_server_confirmed_suffixed_name() {
    // Causality (#3692 review HIGH-1): the daemon applied `tm-new-2` (the
    // requested `tm-new` collided and was auto-suffixed). Pre-fix code never
    // read the body and reported `tm-new` — the message MUST carry the
    // daemon-confirmed name instead.
    let url = spawn_mock_rename_daemon(
        "200 OK",
        r#"{"id":"sess-1","name":"tm-new-2","state":"stopped"}"#,
    )
    .await;
    let client = reqwest::Client::new();
    let msg = do_rename_request(&client, &url, "sess-1", "sess-1", "tm-new".to_string())
        .await
        .expect("rename succeeds");
    assert!(
        msg.contains("tm-new-2"),
        "must report the daemon's applied name: {msg}"
    );
    assert!(
        msg.contains("was taken"),
        "must note the collision suffix: {msg}"
    );
}

#[tokio::test]
async fn do_rename_request_reports_plain_success() {
    // No collision: the daemon confirms exactly the requested name.
    let url = spawn_mock_rename_daemon(
        "200 OK",
        r#"{"id":"sess-1","name":"tm-new","state":"stopped"}"#,
    )
    .await;
    let client = reqwest::Client::new();
    let msg = do_rename_request(&client, &url, "sess-1", "sess-1", "tm-new".to_string())
        .await
        .expect("rename succeeds");
    assert_eq!(msg, "renamed sess-1 -> tm-new");
}
