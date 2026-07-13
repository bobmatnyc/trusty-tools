//! Unit tests for the Slack adapter's pure logic.
//!
//! Why: the adapter's correctness-critical pieces — Socket-Mode envelope parsing,
//! the bot-message guard (no reply loops), the rolling-history cap, the action
//! footer, and dotenv token resolution — are all pure and must be tested without
//! a live Slack socket or the daemon (the live WebSocket loop is deferred to a
//! real Slack app; see the PR body).
//! What: covers `parse_envelope`, `ack_frame`, `record_chat_turn`,
//! `action_footer`, and `resolve_token`.
//! Test: this IS the test module.

use super::*;

#[test]
fn parse_envelope_slash_command() {
    let raw = r#"{
        "type": "slash_commands",
        "envelope_id": "env-1",
        "payload": {
            "command": "/fleet",
            "text": "",
            "channel_id": "C123"
        }
    }"#;
    assert_eq!(
        parse_envelope(raw),
        SlackEvent::SlashCommand {
            envelope_id: "env-1".into(),
            command: "/fleet".into(),
            text: String::new(),
            channel: "C123".into(),
        }
    );
}

#[test]
fn parse_envelope_slash_command_with_text() {
    let raw = r#"{
        "type": "slash_commands",
        "envelope_id": "env-2",
        "payload": { "command": "/msend", "text": "sess-1 build now", "channel_id": "C9" }
    }"#;
    assert_eq!(
        parse_envelope(raw),
        SlackEvent::SlashCommand {
            envelope_id: "env-2".into(),
            command: "/msend".into(),
            text: "sess-1 build now".into(),
            channel: "C9".into(),
        }
    );
}

#[test]
fn parse_envelope_message() {
    let raw = r#"{
        "type": "events_api",
        "envelope_id": "env-3",
        "payload": {
            "event": { "type": "message", "text": "spin up a session", "channel": "C7", "user": "U1" }
        }
    }"#;
    assert_eq!(
        parse_envelope(raw),
        SlackEvent::Message {
            envelope_id: "env-3".into(),
            text: "spin up a session".into(),
            channel: "C7".into(),
        }
    );
}

#[test]
fn parse_envelope_ignores_bot_message() {
    // A message carrying a bot_id is the bot's own post — ignore it (no reply
    // loops) but still surface the envelope id so the loop ACKs it.
    let raw = r#"{
        "type": "events_api",
        "envelope_id": "env-4",
        "payload": {
            "event": { "type": "message", "text": "hi", "channel": "C7", "bot_id": "B1" }
        }
    }"#;
    assert_eq!(
        parse_envelope(raw),
        SlackEvent::Ignored {
            envelope_id: Some("env-4".into())
        }
    );
}

#[test]
fn parse_envelope_ignores_subtype_bot_message() {
    let raw = r#"{
        "type": "events_api",
        "envelope_id": "env-5",
        "payload": {
            "event": { "type": "message", "subtype": "bot_message", "text": "hi", "channel": "C7" }
        }
    }"#;
    assert_eq!(
        parse_envelope(raw),
        SlackEvent::Ignored {
            envelope_id: Some("env-5".into())
        }
    );
}

#[test]
fn parse_envelope_ignores_empty_message() {
    let raw = r#"{
        "type": "events_api",
        "envelope_id": "env-6",
        "payload": { "event": { "type": "message", "text": "   ", "channel": "C7" } }
    }"#;
    assert_eq!(
        parse_envelope(raw),
        SlackEvent::Ignored {
            envelope_id: Some("env-6".into())
        }
    );
}

#[test]
fn parse_envelope_hello_is_ignored() {
    // The `hello` envelope (sent on connect) needs no ACK and no reply.
    let raw = r#"{ "type": "hello" }"#;
    assert_eq!(
        parse_envelope(raw),
        SlackEvent::Ignored { envelope_id: None }
    );
}

#[test]
fn parse_envelope_disconnect_surfaces_reason() {
    // A `disconnect` envelope now surfaces its reason so the loop can classify it
    // (permanent → stop, transient → reconnect) rather than silently ignoring it.
    let raw = r#"{ "type": "disconnect", "reason": "refresh_requested" }"#;
    assert_eq!(
        parse_envelope(raw),
        SlackEvent::Disconnect {
            reason: "refresh_requested".into()
        }
    );
    // A reason-less disconnect surfaces an empty reason (classified transient).
    let raw = r#"{ "type": "disconnect" }"#;
    assert_eq!(
        parse_envelope(raw),
        SlackEvent::Disconnect {
            reason: String::new()
        }
    );
}

#[test]
fn classify_disconnect_reason_permanent_vs_transient() {
    // Permanent: the connection can never be re-established / creds are dead.
    for reason in [
        "app_deactivated",
        "invalid_auth",
        "account_inactive",
        "token_revoked",
        "token_expired",
        "not_authed",
        "missing_scope",
        "no_permission",
    ] {
        assert_eq!(
            classify_disconnect_reason(reason),
            DisconnectKind::Permanent,
            "expected `{reason}` to be permanent",
        );
    }
    // Permanent reasons match case-insensitively and inside a wrapped error string
    // (e.g. the `apps.connections.open failed: …` context).
    assert_eq!(
        classify_disconnect_reason("apps.connections.open failed: INVALID_AUTH"),
        DisconnectKind::Permanent
    );
    // Transient: routine recycles and unknown reasons reconnect with backoff.
    for reason in [
        "refresh_requested",
        "warning",
        "link_disabled",
        "",
        "socket gone",
    ] {
        assert_eq!(
            classify_disconnect_reason(reason),
            DisconnectKind::Transient,
            "expected `{reason}` to be transient",
        );
    }
}

#[test]
fn next_backoff_doubles_and_caps() {
    use std::time::Duration;
    let cap = Duration::from_secs(60);
    // Doubles below the cap.
    assert_eq!(
        next_backoff(Duration::from_secs(2), cap),
        Duration::from_secs(4)
    );
    assert_eq!(
        next_backoff(Duration::from_secs(4), cap),
        Duration::from_secs(8)
    );
    // Saturates at the cap and never exceeds it.
    assert_eq!(next_backoff(Duration::from_secs(40), cap), cap);
    assert_eq!(next_backoff(cap, cap), cap);
    // Never overflows near MAX.
    assert_eq!(next_backoff(Duration::MAX, cap), cap);
}

#[test]
fn parse_envelope_garbage_is_ignored() {
    assert_eq!(
        parse_envelope("not json"),
        SlackEvent::Ignored { envelope_id: None }
    );
}

#[test]
fn ack_frame_shape() {
    assert_eq!(ack_frame("env-1"), r#"{"envelope_id":"env-1"}"#);
}

#[test]
fn envelope_id_of_extracts_each_variant() {
    assert_eq!(
        envelope_id_of(&SlackEvent::SlashCommand {
            envelope_id: "a".into(),
            command: "/x".into(),
            text: String::new(),
            channel: "C".into(),
        }),
        Some("a".to_string())
    );
    assert_eq!(
        envelope_id_of(&SlackEvent::Message {
            envelope_id: "b".into(),
            text: "hi".into(),
            channel: "C".into(),
        }),
        Some("b".to_string())
    );
    assert_eq!(
        envelope_id_of(&SlackEvent::Ignored {
            envelope_id: Some("c".into())
        }),
        Some("c".to_string())
    );
    assert_eq!(
        envelope_id_of(&SlackEvent::Ignored { envelope_id: None }),
        None
    );
}

#[test]
fn record_chat_turn_caps_history() {
    let mut entry: Vec<ChatMessage> = Vec::new();
    // Each call adds a user + assistant turn (2 messages), so pushing
    // MAX_CHAT_HISTORY_TURNS full pairs writes 2*MAX messages — strictly MORE
    // than the cap. This guarantees the test fails if the cap were removed.
    let turns = MAX_CHAT_HISTORY_TURNS;
    let messages_pushed = 2 * turns;
    for i in 0..turns {
        record_chat_turn(&mut entry, &format!("u{i}"), &format!("a{i}"));
    }
    assert!(
        messages_pushed > MAX_CHAT_HISTORY_TURNS,
        "sanity: we must push MORE than the cap so this test is not vacuous"
    );
    // EXACT equality: the length is clamped precisely to the cap, not merely <=.
    assert_eq!(entry.len(), MAX_CHAT_HISTORY_TURNS);
    // The oldest non-evicted message: we pushed [u0,a0,...,u{N-1},a{N-1}] (2N
    // msgs) and drained the front 2N-N = N messages, so the first survivor is
    // the user turn at index N/2 (= MAX_CHAT_HISTORY_TURNS/2).
    let first_retained_turn = MAX_CHAT_HISTORY_TURNS / 2;
    assert_eq!(entry[0].role, "user");
    assert_eq!(entry[0].content, format!("u{first_retained_turn}"));
    // And everything older than that was evicted.
    assert!(
        !entry
            .iter()
            .any(|m| m.content == format!("u{}", first_retained_turn - 1)),
        "the turn before the first retained one must have been evicted"
    );
    // The most recent user message survived.
    let last_user = entry
        .iter()
        .rev()
        .find_map(|m| (m.role == "user").then(|| m.content.clone()));
    assert_eq!(
        last_user.as_deref(),
        Some(&format!("u{}", MAX_CHAT_HISTORY_TURNS - 1)[..])
    );
}

#[test]
fn pid_file_guard_removes_on_drop() {
    // Why: the guard must remove the PID file on EVERY exit path of `run`.
    // Test: create a temp PID file, drop the guard, assert the file is gone.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("slack.pid");
    std::fs::write(&path, "12345").expect("write pid");
    assert!(path.exists(), "precondition: PID file exists before drop");
    {
        let _guard = PidFileGuard { path: path.clone() };
    } // guard drops here
    assert!(!path.exists(), "guard must remove the PID file on drop");
}

#[test]
fn pid_file_guard_drop_missing_is_silent() {
    // Why: a `tm slack stop` may have already removed the file before `run`
    // returns; the guard's drop must be a no-op (best-effort), never a panic.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("slack.pid");
    // No file written — drop must tolerate a missing file silently.
    {
        let _guard = PidFileGuard { path: path.clone() };
    }
    assert!(!path.exists(), "missing PID file stays absent, no panic");
}

#[test]
fn record_chat_turn_skips_empty_reply() {
    let mut entry: Vec<ChatMessage> = Vec::new();
    record_chat_turn(&mut entry, "hello", "");
    // Only the user turn is recorded; an empty assistant reply is skipped.
    assert_eq!(entry.len(), 1);
    assert_eq!(entry[0].role, "user");
}

#[test]
fn action_footer_lists_verbs() {
    let footer = action_footer(Some(&["sessions.list".into(), "sessions.health".into()]));
    assert_eq!(
        footer.as_deref(),
        Some("\n\n_ran: sessions.list, sessions.health_")
    );
}

#[test]
fn action_footer_absent_when_empty() {
    assert_eq!(action_footer(None), None);
    let empty: Vec<String> = Vec::new();
    assert_eq!(action_footer(Some(&empty)), None);
}

#[test]
fn resolve_token_reads_dotenv() {
    use std::io::Write;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(".env.local");
    let mut f = std::fs::File::create(&path).expect("create dotenv");
    writeln!(f, "# a comment").unwrap();
    writeln!(f, "SLACK_BOT_TOKEN=\"xoxb-secret\"").unwrap();
    drop(f);

    // read_dotenv_key is the testable core (resolve_token reads from the cwd).
    assert_eq!(
        read_dotenv_key(&path, "SLACK_BOT_TOKEN").as_deref(),
        Some("xoxb-secret")
    );
    assert_eq!(read_dotenv_key(&path, "MISSING"), None);
}

#[test]
fn resolve_token_missing_is_none() {
    let missing = std::path::Path::new("/nonexistent/.env.local");
    assert_eq!(read_dotenv_key(missing, "SLACK_BOT_TOKEN"), None);
}

#[test]
fn pid_file_path_is_under_framework_root() {
    // The PID file lives under the framework root so start (writer) and stop
    // (reader) agree on one location.
    let path = pid_file_path();
    assert!(path.ends_with("slack.pid"));
    assert!(
        path.to_string_lossy()
            .contains(crate::core::paths::FRAMEWORK_DIR_NAME)
    );
}

#[test]
fn stop_via_pid_file_missing_is_not_running() {
    // No PID file → the bot is simply not running (not an error).
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("slack.pid");
    assert_eq!(stop_via_pid_file(&path), StopOutcome::NotRunning);
}

#[test]
fn stop_via_pid_file_garbage_is_failed_and_removes_file() {
    // A corrupt PID file is a real failure, and the file is cleaned up so the
    // next start is not poisoned.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("slack.pid");
    std::fs::write(&path, "not-a-pid").expect("write pid");
    match stop_via_pid_file(&path) {
        StopOutcome::Failed(_) => {}
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(!path.exists(), "corrupt PID file should be removed");
}

#[cfg(unix)]
#[test]
fn stop_via_pid_file_stale_pid_is_not_running() {
    // A recorded PID that no longer exists is a stale file, not a failure; the
    // file is still removed. We use PID 999999999 which is far beyond any live
    // process (kill returns ESRCH → NotRunning).
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("slack.pid");
    std::fs::write(&path, "999999999").expect("write pid");
    assert_eq!(stop_via_pid_file(&path), StopOutcome::NotRunning);
    assert!(!path.exists(), "stale PID file should be removed");
}

#[tokio::test]
async fn build_slack_client_bounds_a_stalled_connection() {
    // Why (#2517): the bot's Slack-API `reqwest::Client` used to be a bare
    // `reqwest::Client::new()` with no timeout at all. Drives a real request
    // against a `TcpListener` that accepts but never answers (mirrors
    // `client::http_client::config::tests::build_client_bounds_a_stalled_connection`),
    // using tiny test-only bounds so the assertion doesn't have to wait out
    // the real 10s/30s production values.
    // What: builds a client via [`build_slack_client`] with 200ms connect /
    // 300ms request bounds, issues a GET against the stalled listener, and
    // asserts the call errors well within a generous CI margin.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stalling listener");
    let addr = listener.local_addr().expect("read local_addr");

    let client = build_slack_client(
        std::time::Duration::from_millis(200),
        std::time::Duration::from_millis(300),
    );
    let url = format!("http://{addr}/");

    let start = std::time::Instant::now();
    let result = client.get(&url).send().await;
    let elapsed = start.elapsed();

    assert!(result.is_err(), "expected the stalled request to time out");
    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "request took {elapsed:?}, expected it to be bounded by the ~300ms timeout"
    );
}

#[test]
fn slack_client_timeout_constants_are_finite() {
    // Why (#2517): pins the production constants so a future edit can't
    // silently widen them back toward "unbounded" without a visible test
    // failure — the exact regression this module exists to prevent.
    assert_eq!(SLACK_CONNECT_TIMEOUT_SECS, 10);
    assert_eq!(SLACK_REQUEST_TIMEOUT_SECS, 30);
}
