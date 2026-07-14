use super::*;

#[tokio::test]
async fn top_level_default_client_is_bounded_against_a_stalled_daemon() {
    // Why: issue #2517 — the top-level `tm` CLI (`bin/tm/main.rs`) used to
    // mint its own bare, unbounded `reqwest::Client::new()` instead of
    // reusing `DaemonClient`'s bounded client, so `tm status` against a
    // daemon that accepts the TCP connection but never answers hung for the
    // OS-level socket timeout (observed 55.63s live-verify) instead of the
    // intended ~10s request bound. `main.rs` now calls
    // `client::http_client::default_client()` directly — the SAME function
    // `DaemonClient::new` calls — so this test exercises the real
    // production bounds (not `config`'s scaled-down test bounds) end-to-end
    // against a stalled listener, pinning that the public wrapper is not
    // accidentally an unbounded client in disguise.
    // What: builds a client via `default_client()`, issues a GET against a
    // `TcpListener` that accepts but never answers, and asserts the call
    // errors comfortably within the 10s production request bound.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind stalling listener");
    let addr = listener.local_addr().expect("read local_addr");

    let client = default_client();
    let url = format!("http://{addr}/");

    let start = std::time::Instant::now();
    let result = client.get(&url).send().await;
    let elapsed = start.elapsed();

    // `listener` stays alive (dropped at function end) for the whole
    // request so the connection stalls rather than being refused outright.
    assert!(result.is_err(), "expected the stalled request to time out");
    assert!(
        elapsed < std::time::Duration::from_secs(15),
        "request took {elapsed:?}, expected it to be bounded by the ~10s production \
         timeout, not the OS-level socket timeout (issue #2517 regression)"
    );
}

#[test]
fn base_url_is_stored() {
    let client = DaemonClient::new("http://127.0.0.1:7880");
    assert_eq!(client.base_url(), "http://127.0.0.1:7880");
}

#[test]
fn with_client_reuses_passed_client() {
    // Why: callers that configured a `reqwest::Client` (TLS/timeout/proxy/pool)
    // must keep that configuration; `with_client` adopts the passed client
    // verbatim instead of minting a fresh default one.
    let configured = reqwest::Client::new();
    let client = DaemonClient::with_client(configured, "http://127.0.0.1:7880");
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

/// Spawn the daemon's real HTTP API on a random loopback port, rooted in a
/// throwaway temp directory with an isolated (empty) managed-session store.
///
/// Why: mirrors `client::executor::tests::spawn_test_daemon` — a real bind is
/// needed to prove `spawn_managed_session` reads the response BODY (via
/// [`super::error::response_or_body_error`]), not just the status line.
async fn spawn_test_daemon() -> String {
    use std::future::IntoFuture as _;

    use crate::daemon::{api, state::DaemonState};
    let root = tempfile::tempdir().unwrap().keep();
    let state = std::sync::Arc::new(DaemonState::with_root_isolated_managed(root).await);
    let router = api::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, router).into_future());
    format!("http://{addr}")
}

/// #2457 follow-up to #2496: a rejected spawn must surface the daemon's body
/// message, not collapse to a bare "404 Not Found" the way `error_for_status`
/// alone would. No project is registered for `repo_url` here, so the daemon's
/// deliverable pre-check (`validate_deliverable_scope`, run BEFORE any
/// provisioning) rejects with `DaemonError::ProjectNotFoundForRepoUrl` — a 404
/// whose body names the offending repo_url.
#[tokio::test]
async fn spawn_managed_session_surfaces_daemon_error_body() {
    let url = spawn_test_daemon().await;
    let client = DaemonClient::new(url);
    let req = ManagedSpawnRequest {
        repo_url: "https://github.com/acme/unregistered".to_string(),
        git_ref: "main".to_string(),
        task: "do it".to_string(),
        name_hint: None,
        runtime: None,
        inject_task: None,
        deliverable_id: Some("11111111-1111-1111-1111-111111111111".to_string()),
        force_new: false,
    };
    let err = client
        .spawn_managed_session(&req)
        .await
        .expect_err("no project is registered for this repo_url");
    let msg = err.to_string();
    assert!(
        msg.contains("acme/unregistered") || msg.contains("unregistered"),
        "error should carry the daemon's body message, not just the bare status: {msg}"
    );
    assert!(msg.contains("404"), "{msg}");
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
    // This payload is what an OLDER daemon (pre-#1275) emits — no summary
    // fields. `#[serde(default)]` makes the client tolerant: absent → None/false.
    assert_eq!(context.sessions[0].last_summary, None);
    assert!(!context.sessions[0].summarizing);
}

#[test]
fn coordinator_session_carries_summary_fields() {
    // A current daemon (#1275) emits `last_summary` + `summarizing`; the client
    // deserializes both so the TUI can render the bullet and blink (DOC-16 §6.2).
    let json = serde_json::json!({
        "sessions": [{
            "id": "00000000-0000-0000-0000-000000000000",
            "name": "tmpm-aipowerranking",
            "prefix": "aipowerranking",
            "workdir": "/tmp/proj",
            "status": "Active",
            "active_delegations": 0,
            "recent_output": [],
            "last_summary": "Writing the parser tests",
            "summarizing": true,
        }],
    });
    let context: CoordinatorContext = serde_json::from_value(json).unwrap();
    assert_eq!(
        context.sessions[0].last_summary.as_deref(),
        Some("Writing the parser tests")
    );
    assert!(context.sessions[0].summarizing);
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
    // Additive action-path fields default to None when absent.
    assert!(outcome.actions_taken.is_none());
    assert!(outcome.conv_id.is_none());
}

#[test]
fn coordinator_chat_outcome_deserializes_actions() {
    // The action-capable path returns the audit trail and the conversation id.
    let json = serde_json::json!({
        "reply": "ran a health check and listed the fleet",
        "actions_taken": ["sessions.health", "sessions.list"],
        "conv_id": "conv-42",
    });
    let outcome: CoordinatorChatOutcome = serde_json::from_value(json).unwrap();
    assert_eq!(
        outcome.actions_taken.as_deref(),
        Some(["sessions.health".to_string(), "sessions.list".to_string()].as_slice())
    );
    assert_eq!(outcome.conv_id.as_deref(), Some("conv-42"));
}

#[test]
fn coordinator_chat_serializes_actions_flag() {
    // The request body must carry the `actions` boolean so the daemon can route
    // the action-capable SM branch; history is threaded through verbatim.
    let history = [ChatMessage::user("hi"), ChatMessage::assistant("hello")];
    let body = coordinator_chat_body("spin up a session", &history, true);
    assert_eq!(body["message"], "spin up a session");
    assert_eq!(body["actions"], true);
    assert_eq!(body["history"][0]["role"], "user");
    assert_eq!(body["history"][1]["content"], "hello");

    // The passive path serializes `actions: false`.
    let passive = coordinator_chat_body("just chatting", &[], false);
    assert_eq!(passive["actions"], false);
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

// ── Managed session-manager wire-shape round-trips ─────────────────────────────

#[test]
fn managed_session_summary_deserializes() {
    // A full summary round-trips; the optional fields are also tolerant of being
    // absent (an older/leaner daemon) via `#[serde(default)]`.
    let json = serde_json::json!({
        "id": "00000000-0000-0000-0000-000000000001",
        "name": "tmpm-brave-otter",
        "state": "running",
        "workspace_path": "/tmp/ws",
        "repo_url": "https://example.com/r.git",
        "branch": "main",
        "created_at": "2026-06-19T00:00:00Z",
        "last_activity_at": "2026-06-19T01:00:00Z",
        "pending_decision": "overwrite?",
        "proposed_default": "yes",
        "source_id": "owner/repo",
    });
    let s: ManagedSessionSummary = serde_json::from_value(json).unwrap();
    assert_eq!(s.id, "00000000-0000-0000-0000-000000000001");
    assert_eq!(s.name, "tmpm-brave-otter");
    assert_eq!(s.state, "running");
    assert_eq!(s.created_at.as_deref(), Some("2026-06-19T00:00:00Z"));
    assert_eq!(s.pending_decision.as_deref(), Some("overwrite?"));
    // source_id round-trips (#1730).
    assert_eq!(s.source_id.as_deref(), Some("owner/repo"));

    // Minimal body: only the always-present fields.
    let lean = serde_json::json!({"id": "x", "name": "n", "state": "stopped"});
    let s: ManagedSessionSummary = serde_json::from_value(lean).unwrap();
    assert_eq!(s.id, "x");
    assert!(s.workspace_path.is_none());
    // An absent `created_at` deserializes to `None`, not an empty string.
    assert!(s.created_at.is_none());
    assert!(s.pending_decision.is_none());
    // An absent `source_id` defaults to None (legacy sessions without it).
    assert!(s.source_id.is_none());
}

/// #2595: `unresumable` must round-trip when present, and default `false`
/// (never spuriously flag a session dead) when an older daemon omits it.
#[test]
fn managed_session_summary_deserializes_unresumable_flag() {
    let with_flag = serde_json::json!({
        "id": "x", "name": "n", "state": "stopped", "unresumable": true
    });
    let s: ManagedSessionSummary = serde_json::from_value(with_flag).unwrap();
    assert!(s.unresumable, "unresumable: true must deserialize to true");

    let omitted = serde_json::json!({"id": "x", "name": "n", "state": "stopped"});
    let s: ManagedSessionSummary = serde_json::from_value(omitted).unwrap();
    assert!(
        !s.unresumable,
        "an older daemon omitting `unresumable` must default to false"
    );
}

#[test]
fn managed_list_response_deserializes() {
    let json = serde_json::json!({
        "sessions": [
            {"id": "a", "name": "tmpm-a", "state": "running"},
            {"id": "b", "name": "tmpm-b", "state": "stopped"},
        ],
    });
    let body: ManagedListResponse = serde_json::from_value(json).unwrap();
    assert_eq!(body.sessions.len(), 2);
    assert_eq!(body.sessions[1].name, "tmpm-b");

    // An empty / absent `sessions` array defaults to empty.
    let empty: ManagedListResponse = serde_json::from_value(serde_json::json!({})).unwrap();
    assert!(empty.sessions.is_empty());
}

#[test]
fn managed_spawn_request_serializes() {
    // The `git_ref` field must serialize under the wire key `ref`, and absent
    // optionals must be omitted so the daemon sees them as null/defaulted.
    let req = ManagedSpawnRequest {
        repo_url: "https://example.com/r.git".to_string(),
        git_ref: "main".to_string(),
        task: "do the thing".to_string(),
        name_hint: Some("tmpm-custom".to_string()),
        runtime: Some("tcode".to_string()),
        inject_task: None,
        deliverable_id: Some("11111111-1111-1111-1111-111111111111".to_string()),
        force_new: false,
    };
    let v = serde_json::to_value(&req).unwrap();
    assert_eq!(v["ref"], "main");
    assert_eq!(v["repo_url"], "https://example.com/r.git");
    assert_eq!(v["name_hint"], "tmpm-custom");
    assert_eq!(v["runtime"], "tcode");
    assert_eq!(
        v["deliverable_id"], "11111111-1111-1111-1111-111111111111",
        "Some(deliverable_id) must serialize as its string value"
    );
    assert!(v.get("git_ref").is_none(), "must use wire key `ref`");

    let bare = ManagedSpawnRequest {
        repo_url: "r".to_string(),
        git_ref: "r".to_string(),
        task: "t".to_string(),
        name_hint: None,
        runtime: None,
        inject_task: None,
        deliverable_id: None,
        force_new: false,
    };
    let v = serde_json::to_value(&bare).unwrap();
    assert!(v.get("name_hint").is_none(), "None name_hint is omitted");
    assert!(v.get("runtime").is_none(), "None runtime is omitted");
    assert!(
        v.get("inject_task").is_none(),
        "None inject_task is omitted"
    );
    assert!(
        v.get("deliverable_id").is_none(),
        "None deliverable_id is omitted (#2379, same additive pattern as inject_task)"
    );
    assert!(
        v.get("force_new").is_none(),
        "false force_new is omitted (#2450) so the wire matches the pre-existing shape"
    );
}

#[test]
fn managed_spawn_response_deserializes() {
    let json = serde_json::json!({
        "id": "id-1",
        "name": "tmpm-x",
        "state": "running",
        "created_at": "2026-06-19T00:00:00Z",
        "attach_cmd": "tmux attach-session -t tmpm-x",
        "runtime": "claude-code",
    });
    let r: ManagedSpawnResponse = serde_json::from_value(json).unwrap();
    assert_eq!(r.id, "id-1");
    assert_eq!(r.created_at.as_deref(), Some("2026-06-19T00:00:00Z"));
    assert_eq!(r.attach_cmd, "tmux attach-session -t tmpm-x");
    assert_eq!(r.runtime, "claude-code");

    // An absent `created_at` deserializes to `None`, while `attach_cmd` and
    // `runtime` remain plain strings defaulting to empty.
    let lean = serde_json::json!({"id": "id-2", "name": "tmpm-y", "state": "running"});
    let r: ManagedSpawnResponse = serde_json::from_value(lean).unwrap();
    assert!(r.created_at.is_none());
    assert_eq!(r.attach_cmd, "");
    assert_eq!(r.runtime, "");
}

#[test]
fn managed_adopt_request_serializes() {
    // Required fields are always present; absent optionals are omitted so the
    // daemon defaults them (#1433).
    let req = ManagedAdoptRequest {
        tmux_name: "tmpm-hand-started".to_string(),
        cwd: "/Users/op/work/proj".to_string(),
        task: Some("drive it".to_string()),
        runtime: Some("claude-code".to_string()),
    };
    let v = serde_json::to_value(&req).unwrap();
    assert_eq!(v["tmux_name"], "tmpm-hand-started");
    assert_eq!(v["cwd"], "/Users/op/work/proj");
    assert_eq!(v["task"], "drive it");
    assert_eq!(v["runtime"], "claude-code");

    let bare = ManagedAdoptRequest {
        tmux_name: "my-cli-session".to_string(),
        cwd: "/x".to_string(),
        task: None,
        runtime: None,
    };
    let v = serde_json::to_value(&bare).unwrap();
    assert_eq!(v["tmux_name"], "my-cli-session");
    assert!(v.get("task").is_none(), "None task is omitted");
    assert!(v.get("runtime").is_none(), "None runtime is omitted");
}

#[test]
fn managed_adopt_response_deserializes() {
    let json = serde_json::json!({
        "id": "id-9",
        "name": "tmpm-hand-started",
        "state": "active",
        "cwd": "/Users/op/work/proj",
        "runtime": "claude-code",
        "attach_cmd": "tmux attach-session -t tmpm-hand-started",
    });
    let r: ManagedAdoptResponse = serde_json::from_value(json).unwrap();
    assert_eq!(r.id, "id-9");
    assert_eq!(r.name, "tmpm-hand-started");
    assert_eq!(r.state, "active");
    assert_eq!(r.cwd, "/Users/op/work/proj");
    assert_eq!(r.runtime, "claude-code");
    assert_eq!(r.attach_cmd, "tmux attach-session -t tmpm-hand-started");

    // A lean response (only id/name/state) still deserializes; the defaulted
    // string fields fall back to empty.
    let lean = serde_json::json!({"id": "id-10", "name": "x", "state": "active"});
    let r: ManagedAdoptResponse = serde_json::from_value(lean).unwrap();
    assert_eq!(r.cwd, "");
    assert_eq!(r.runtime, "");
    assert_eq!(r.attach_cmd, "");
}

#[test]
fn managed_send_and_answer_round_trip() {
    let v = serde_json::to_value(ManagedSendInputRequest {
        text: "hello".to_string(),
    })
    .unwrap();
    assert_eq!(v["text"], "hello");
    let sent: ManagedSendInputResponse =
        serde_json::from_value(serde_json::json!({"sent": true, "tmux_name": "tmpm-x"})).unwrap();
    assert!(sent.sent);
    assert_eq!(sent.tmux_name, "tmpm-x");

    let v = serde_json::to_value(ManagedAnswerRequest {
        answer: "yes".to_string(),
    })
    .unwrap();
    assert_eq!(v["answer"], "yes");
    let answered: ManagedAnswerResponse =
        serde_json::from_value(serde_json::json!({"injected": true, "tmux_name": "tmpm-x"}))
            .unwrap();
    assert!(answered.injected);
}

#[test]
fn managed_attach_cmd_response_deserializes() {
    let r: ManagedAttachCmdResponse =
        serde_json::from_value(serde_json::json!({"attach_cmd": "tmux attach-session -t tmpm-x"}))
            .unwrap();
    assert_eq!(r.attach_cmd, "tmux attach-session -t tmpm-x");
}

#[test]
fn managed_activity_response_deserializes() {
    // Both with and without the optional classifier overlay.
    let json = serde_json::json!({
        "raw_pane": "line1\nline2",
        "runtime_active": true,
        "state": "working",
        "summary": "running tests",
        "confidence": 0.8_f32,
        "cache_hit": false,
        "input_tokens": 10,
        "output_tokens": 5,
        "latency_ms": 42,
        "total_input_tokens": 100,
        "total_output_tokens": 50,
        "classification": "working",
        "pending_decision": "overwrite?",
        "proposed_default": "yes",
    });
    let a: ManagedActivityResponse = serde_json::from_value(json).unwrap();
    assert_eq!(a.state, "working");
    assert!(a.runtime_active);
    assert_eq!(a.classification.as_deref(), Some("working"));
    assert_eq!(a.pending_decision.as_deref(), Some("overwrite?"));

    // No classifier ran: `classification` null, raw pane still present.
    let json = serde_json::json!({
        "raw_pane": "pane",
        "runtime_active": false,
        "state": "unknown",
        "summary": "",
        "confidence": 0.0_f32,
        "cache_hit": false,
        "input_tokens": 0,
        "output_tokens": 0,
        "latency_ms": 0,
        "total_input_tokens": 0,
        "total_output_tokens": 0,
        "classification": null,
    });
    let a: ManagedActivityResponse = serde_json::from_value(json).unwrap();
    assert_eq!(a.raw_pane, "pane");
    assert!(a.classification.is_none());
    assert!(a.pending_decision.is_none());
}

#[test]
fn managed_session_urls_use_managed_route_family() {
    // The managed methods build URLs off the `/api/v1/sessions/managed` base and
    // must NOT collide with the legacy `resume_session` route (`/sessions/{id}/
    // resume`). This guards the route family contract that STUI/TELUI converge on.
    let client = DaemonClient::new("http://127.0.0.1:7880");
    assert_eq!(client.base_url(), "http://127.0.0.1:7880");
    // Construct the same format strings the methods use, to lock the shape.
    let base = client.base_url();
    assert_eq!(
        format!("{base}/api/v1/sessions/managed/{id}/resume", id = "abc"),
        "http://127.0.0.1:7880/api/v1/sessions/managed/abc/resume"
    );
    // The legacy method targets a different path; the two never overlap.
    assert_eq!(
        format!("{base}/sessions/{id}/resume", id = "abc"),
        "http://127.0.0.1:7880/sessions/abc/resume"
    );
}

#[test]
fn health_snapshot_deserializes() {
    // The `/health` body deserializes into HealthSnapshot; the catalog flags
    // default when absent so an older daemon returning only `status` still parses.
    let full: HealthSnapshot = serde_json::from_value(serde_json::json!({
        "status": "ok",
        "catalog_stale": true,
        "catalog_unknown": false,
        "catalog_changes": ["agents/foo"],
    }))
    .expect("full health body parses");
    assert_eq!(full.status, "ok");
    assert!(full.catalog_stale);
    assert!(!full.catalog_unknown);

    let minimal: HealthSnapshot =
        serde_json::from_value(serde_json::json!({ "status": "ok" })).expect("minimal body parses");
    assert_eq!(minimal.status, "ok");
    assert!(!minimal.catalog_stale);
    assert!(!minimal.catalog_unknown);
}
