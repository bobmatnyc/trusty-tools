use super::*;

#[test]
fn base_url_is_stored() {
    let client = DaemonClient::new("http://127.0.0.1:7880");
    assert_eq!(client.base_url(), "http://127.0.0.1:7880");
}

#[test]
fn set_base_url_repoints_client() {
    // Why: a long-lived UI must follow the daemon to a new ephemeral port
    // after a restart; `set_base_url` is what makes that re-pointing possible.
    let mut client = DaemonClient::new("http://127.0.0.1:7880");
    client.set_base_url("http://127.0.0.1:54321");
    assert_eq!(client.base_url(), "http://127.0.0.1:54321");
}

#[tokio::test]
async fn launch_session_errors_when_daemon_unreachable() {
    // Why: `/connect <dir>` launches via `launch_session`; when the daemon
    // POST fails (port 0 never connects) the error must surface rather than
    // proceeding to spawn tmux against an unregistered session.
    let client = DaemonClient::new("http://127.0.0.1:0");
    let result = client.launch_session("/tmp/no-such-project").await;
    assert!(result.is_err(), "expected launch to fail with no daemon");
}

#[tokio::test]
async fn connect_session_errors_when_daemon_unreachable() {
    // Why: `tm connect` registers via `POST /api/v1/sessions/connect`
    // before touching tmux; when the daemon POST fails the error must
    // surface rather than proceeding to spawn tmux against an
    // unregistered session.
    let client = DaemonClient::new("http://127.0.0.1:0");
    let result = client.connect_session("/tmp/no-such-project").await;
    assert!(result.is_err(), "expected connect to fail with no daemon");
}

#[test]
fn session_row_deserializes_tmux_name() {
    let json = serde_json::json!({
        "id": "abcd1234-5678-90ab-cdef-1234567890ab",
        "workdir": "/tmp/proj",
        "status": "Active",
        "active_delegations": 1,
        "tmux_name": "tmpm-quiet-falcon"
    });
    let row: SessionRow = serde_json::from_value(json).unwrap();
    assert_eq!(row.tmux_name, "tmpm-quiet-falcon");
}

#[test]
fn session_row_defaults_tmux_name_when_absent() {
    let json = serde_json::json!({
        "id": "abcd1234-5678-90ab-cdef-1234567890ab",
        "workdir": "/tmp/proj",
        "status": "Active"
    });
    let row: SessionRow = serde_json::from_value(json).unwrap();
    assert_eq!(row.tmux_name, "");
    assert_eq!(row.last_seen.secs_since_epoch, 0);
}

#[test]
fn events_deserialize_from_record_shape() {
    let json = serde_json::json!({
        "session": "abcd1234-5678-90ab-cdef-1234567890ab",
        "event": "PreToolUse",
        "at": "2024-01-01T00:00:00Z",
        "payload": {}
    });
    let row: EventRow = serde_json::from_value(json).unwrap();
    assert_eq!(row.event, crate::core::hook::HookEvent::PreToolUse);
    assert_eq!(row.at, "2024-01-01T00:00:00Z");
}

#[test]
fn events_default_payload_when_absent() {
    let json = serde_json::json!({
        "session": "abcd1234-5678-90ab-cdef-1234567890ab",
        "event": "Stop",
        "at": "2024-01-01T00:00:00Z"
    });
    let row: EventRow = serde_json::from_value(json).unwrap();
    assert!(row.payload.is_null());
}

#[test]
fn breakers_deserialize_from_api_shape() {
    let json = serde_json::json!({
        "agent": "research",
        "breaker": { "state": "closed", "consecutive_failures": 0 }
    });
    #[derive(serde::Deserialize)]
    struct WireBreaker {
        state: String,
        consecutive_failures: u32,
    }
    #[derive(serde::Deserialize)]
    struct WireRow {
        agent: String,
        breaker: WireBreaker,
    }
    let row: WireRow = serde_json::from_value(json).unwrap();
    assert_eq!(row.agent, "research");
    assert_eq!(row.breaker.state, "closed");
    assert_eq!(row.breaker.consecutive_failures, 0);
}

#[test]
fn tmux_session_row_accepts_name() {
    // The snapshot helper joins a `lines` array; the name parse is exercised
    // here directly on both wire shapes.
    let obj = serde_json::json!({"name": "tmpm-quiet-falcon"});
    assert_eq!(
        obj.get("name").and_then(|v| v.as_str()),
        Some("tmpm-quiet-falcon")
    );
    let plain = serde_json::json!("external-shell");
    assert_eq!(plain.as_str(), Some("external-shell"));
}

#[test]
fn snapshot_text_handles_each_shape() {
    assert_eq!(snapshot_text(&serde_json::json!("plain")), "plain");
    assert_eq!(
        snapshot_text(&serde_json::json!({"content": "from content"})),
        "from content"
    );
    assert_eq!(
        snapshot_text(&serde_json::json!({"lines": ["a", "b"]})),
        "a\nb"
    );
}

#[test]
fn pair_request_deserializes() {
    let json = serde_json::json!({"code": "A4X9KZ", "expires_in_seconds": 300});
    let req: PairRequest = serde_json::from_value(json).unwrap();
    assert_eq!(req.code, "A4X9KZ");
    assert_eq!(req.expires_in_seconds, 300);
}

#[test]
fn pair_confirm_deserializes_failure() {
    let json = serde_json::json!({"success": false, "error": "invalid or expired code"});
    let confirm: PairConfirm = serde_json::from_value(json).unwrap();
    assert!(!confirm.success);
    assert_eq!(confirm.error.as_deref(), Some("invalid or expired code"));
    assert_eq!(confirm.chat_id, None);
}

#[test]
fn llm_chat_message_round_trips() {
    // A ChatMessage serializes to the `{role, content}` wire shape the
    // daemon expects and deserializes back unchanged.
    let msg = ChatMessage {
        role: "user".into(),
        content: "hello".into(),
    };
    let json = serde_json::to_value(&msg).unwrap();
    assert_eq!(json["role"], "user");
    assert_eq!(json["content"], "hello");
    let back: ChatMessage = serde_json::from_value(json).unwrap();
    assert_eq!(back, msg);
}

#[test]
fn chat_message_constructors_set_role() {
    assert_eq!(ChatMessage::user("x").role, "user");
    assert_eq!(ChatMessage::assistant("y").role, "assistant");
    assert_eq!(ChatMessage::user("x").content, "x");
}

#[test]
fn llm_chat_response_deserializes() {
    // The `POST /llm/chat` response carries the reply and updated history.
    let json = serde_json::json!({
        "reply": "hi there",
        "history": [
            { "role": "user", "content": "hello" },
            { "role": "assistant", "content": "hi there" },
        ],
    });
    let outcome: LlmChatOutcome = serde_json::from_value(json).unwrap();
    assert_eq!(outcome.reply, "hi there");
    assert_eq!(outcome.history.len(), 2);
    assert_eq!(outcome.history[1].role, "assistant");
}

#[test]
fn coordinator_context_deserializes() {
    // The `GET /api/v1/sessions/context` snapshot carries the session
    // summaries; the daemon's `recent_events` field is ignored.
    let json = serde_json::json!({
        "sessions": [{
            "id": "00000000-0000-0000-0000-000000000000",
            "name": "tmpm-aipowerranking",
            "prefix": "aipowerranking",
            "workdir": "/tmp/proj",
            "status": "Active",
            "active_delegations": 3,
            "recent_output": ["building…"],
        }],
        "recent_events": [],
        "generated_at": "2026-05-19T00:00:00Z",
    });
    let context: CoordinatorContext = serde_json::from_value(json).unwrap();
    assert_eq!(context.sessions.len(), 1);
    assert_eq!(context.sessions[0].prefix, "aipowerranking");
    assert_eq!(context.sessions[0].active_delegations, 3);
}

#[test]
fn coordinator_chat_outcome_deserializes() {
    // A routed-command outcome carries the session name and pane output.
    let json = serde_json::json!({
        "reply": "Sent to tmpm-foo: run tests",
        "routed_to_session": "tmpm-foo",
        "command_output": "tests passed",
    });
    let outcome: CoordinatorChatOutcome = serde_json::from_value(json).unwrap();
    assert_eq!(outcome.routed_to_session.as_deref(), Some("tmpm-foo"));
    assert_eq!(outcome.command_output.as_deref(), Some("tests passed"));

    // A plain LLM reply omits the routing fields.
    let json = serde_json::json!({ "reply": "two sessions are active" });
    let outcome: CoordinatorChatOutcome = serde_json::from_value(json).unwrap();
    assert_eq!(outcome.reply, "two sessions are active");
    assert!(outcome.routed_to_session.is_none());
}

#[test]
fn pair_status_deserializes() {
    let json = serde_json::json!({"paired": true, "chat_id": 12345678});
    let status: PairStatus = serde_json::from_value(json).unwrap();
    assert!(status.paired);
    assert_eq!(status.chat_id, Some(12345678));
}

#[test]
fn catalog_stale_health_body_wire_shape() {
    // The HR-3 `catalog_stale` flag must round-trip from the `/health` body, and
    // a body MISSING the field (an older daemon) must default to `false` so
    // `DaemonClient::catalog_stale` degrades to "no updates" instead of erroring.
    // This mirrors the private `HealthBody` the client parses.
    #[derive(serde::Deserialize)]
    struct HealthBody {
        #[serde(default)]
        catalog_stale: bool,
    }

    let stale: HealthBody =
        serde_json::from_value(serde_json::json!({"status":"ok","catalog_stale":true})).unwrap();
    assert!(stale.catalog_stale);

    let fresh: HealthBody =
        serde_json::from_value(serde_json::json!({"status":"ok","catalog_stale":false})).unwrap();
    assert!(!fresh.catalog_stale);

    // Legacy daemon: no `catalog_stale` field present → defaults to false.
    let legacy: HealthBody = serde_json::from_value(serde_json::json!({"status":"ok"})).unwrap();
    assert!(!legacy.catalog_stale, "missing field defaults to false");
}
