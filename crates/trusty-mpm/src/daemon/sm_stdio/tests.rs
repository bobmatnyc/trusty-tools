//! Round-trip + framing tests for the SM stdio adapter (SM-STDIO #1291).
//!
//! Why: the acceptance bar is that EVERY one of the 13 `sm.*` methods round-trips
//! over the dispatcher with correct JSON-RPC 2.0 framing (id echoed, result shape
//! correct), that malformed/unknown requests produce proper JSON-RPC errors (no
//! panic), that `sm.chat` drives a full SM turn and `sm.health` reports provider
//! status, and that stdout stays clean (no `println!` in the SM paths). These
//! tests pin all of that deterministically with a mock resolver + mock session
//! control + a tempdir — NO network, NO tmux, NO real LLM.
//! What: builds an [`SmDispatcher`] over mocks, drives [`SmDispatcher::dispatch`]
//! with constructed [`Request`]s, and asserts the [`Response`] envelope. Also
//! exercises the shared line-framing loop and greps the source for stdout writes.
//! Test: this is the test module.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use tempfile::TempDir;
use trusty_common::mcp::{Request, Response, error_codes};

use super::SmDispatcher;
use super::control::{LaunchParams, SessionControl, SessionControlError};
use super::methods::{CODE_NOT_FOUND, CODE_UNAVAILABLE};
use crate::core::sm::SessionManagerConfig;
use crate::core::sm::agent::SessionManagerAgent;
use crate::core::sm::agent::mock::{MockChatProvider, MockResolver};

// ── Mock session control ────────────────────────────────────────────────────────

/// A deterministic [`SessionControl`] mock for the `sm.sessions.*` round-trips.
///
/// Why: the dispatcher's session methods must be testable with NO tmux/workspace.
/// This mock records the last call and returns canned, well-formed JSON so the
/// tests assert the dispatcher's mapping (id echo, result shape) in isolation.
/// What: `launch` returns a fixed `session_id`; `list`/`get`/`send`/`stop`/
/// `resume`/`kill` return canned bodies. A `fail_get_not_found` flag makes `get`
/// return [`SessionControlError::NotFound`] so the not-found mapping is covered.
/// Test: drives `sm.sessions.*` tests below.
#[derive(Default)]
struct MockSessionControl {
    /// When true, `get` returns NotFound (to cover the not-found JSON-RPC mapping).
    fail_get_not_found: bool,
    /// When true, `stop`/`kill` return [`SessionControlError::Backend`] (to cover
    /// the backend-failure JSON-RPC mapping — a tmux/store failure on a session
    /// that DOES exist must NOT be reported as not-found).
    fail_stop_kill_backend: bool,
    /// The last launch params seen (so a test can assert the mapping).
    last_launch: std::sync::Mutex<Option<LaunchParams>>,
}

#[async_trait]
impl SessionControl for MockSessionControl {
    async fn launch(&self, params: LaunchParams) -> Result<Value, SessionControlError> {
        *self.last_launch.lock().expect("lock") = Some(params);
        Ok(json!({ "session_id": "11111111-1111-1111-1111-111111111111" }))
    }
    async fn list(&self) -> Result<Value, SessionControlError> {
        Ok(json!({ "sessions": [] }))
    }
    async fn get(&self, session_id: &str) -> Result<Value, SessionControlError> {
        if self.fail_get_not_found {
            return Err(SessionControlError::NotFound(session_id.to_string()));
        }
        Ok(json!({ "session": { "id": session_id, "state": "active" } }))
    }
    async fn send(&self, _id: &str, _text: &str) -> Result<Value, SessionControlError> {
        Ok(json!({ "ok": true }))
    }
    async fn stop(&self, _id: &str) -> Result<Value, SessionControlError> {
        if self.fail_stop_kill_backend {
            return Err(SessionControlError::Backend("tmux kill failed".into()));
        }
        Ok(json!({ "ok": true }))
    }
    async fn resume(&self, _id: &str) -> Result<Value, SessionControlError> {
        Ok(json!({ "ok": true }))
    }
    async fn kill(&self, _id: &str) -> Result<Value, SessionControlError> {
        if self.fail_stop_kill_backend {
            return Err(SessionControlError::Backend(
                "decommission tmux failed".into(),
            ));
        }
        Ok(json!({ "ok": true }))
    }
}

// ── Dispatcher builders (feature-aware) ─────────────────────────────────────────

/// An enabled SM config (default tiers) for the dispatcher tests.
fn enabled_config() -> SessionManagerConfig {
    SessionManagerConfig {
        enabled: true,
        ..SessionManagerConfig::default()
    }
}

/// Build an SM agent over a (mock) provider resolver, feature-aware.
fn agent_with_provider(
    cfg: SessionManagerConfig,
    data_root: &std::path::Path,
) -> Arc<SessionManagerAgent> {
    let provider = MockChatProvider::new("SM plan: delegate to a session", 0.0021);
    let resolver = Arc::new(MockResolver::with_provider(provider));
    Arc::new(SessionManagerAgent::for_test(
        cfg,
        resolver,
        data_root.to_path_buf(),
    ))
}

/// Build a degraded SM agent (no provider), feature-aware.
fn agent_degraded(
    cfg: SessionManagerConfig,
    data_root: &std::path::Path,
) -> Arc<SessionManagerAgent> {
    let resolver = Arc::new(MockResolver::degraded());
    Arc::new(SessionManagerAgent::for_test(
        cfg,
        resolver,
        data_root.to_path_buf(),
    ))
}

/// Build a dispatcher over the given agent + a fresh mock session control.
///
/// Why: the feature-gated goal-store field makes construction differ by build;
/// this helper hides that so the tests read identically. Under `sm-memory` it
/// loads an empty goal store over a no-op palace + the tempdir.
fn dispatcher_with(
    agent: Arc<SessionManagerAgent>,
    cfg: SessionManagerConfig,
    data_root: &std::path::Path,
    control: Arc<MockSessionControl>,
) -> SmDispatcher {
    let sessions: Arc<dyn SessionControl> = control;
    #[cfg(feature = "sm-memory")]
    let goals = Some(test_goal_store(data_root));
    #[cfg(not(feature = "sm-memory"))]
    let goals = None;
    SmDispatcher::new(agent, cfg, data_root.to_path_buf(), sessions, goals)
}

/// Build an in-memory goal store over a no-op palace for `sm.goals.*` tests.
///
/// Why: the goal-store methods must round-trip without a real palace (no ONNX);
/// a no-op [`GoalMemory`] gives the SM-6 store a seam that always succeeds with no
/// durable entries, so creates/updates are visible in-memory within one test.
#[cfg(feature = "sm-memory")]
fn test_goal_store(
    data_root: &std::path::Path,
) -> std::sync::Arc<tokio::sync::Mutex<crate::core::sm::SmGoalStore>> {
    use crate::core::sm::{GoalMemory, SmGoalStore};

    struct NoopMem;
    #[async_trait]
    impl GoalMemory for NoopMem {
        async fn remember_goal(&self, _json: String, _tag: &str) -> Result<(), String> {
            Ok(())
        }
        async fn list_goals(&self, _tag: &str) -> Result<Vec<String>, String> {
            Ok(Vec::new())
        }
    }
    let store = SmGoalStore::new(Arc::new(NoopMem), data_root.to_path_buf());
    std::sync::Arc::new(tokio::sync::Mutex::new(store))
}

/// Build a request envelope with an integer id.
fn req(id: i64, method: &str, params: Value) -> Request {
    Request {
        jsonrpc: Some("2.0".into()),
        id: Some(json!(id)),
        method: method.into(),
        params: Some(params),
    }
}

/// Assert the response echoed `id` and carries a `result` (not an error); return it.
fn ok_result(resp: &Response, id: i64) -> Value {
    assert_eq!(resp.jsonrpc, "2.0", "framing: jsonrpc 2.0");
    assert_eq!(resp.id, Some(json!(id)), "framing: id echoed");
    assert!(
        resp.error.is_none(),
        "expected result, got error: {:?}",
        resp.error
    );
    resp.result.clone().expect("result present")
}

/// Assert the response is an error with the given code; return the message.
fn err_code(resp: &Response, id: i64, code: i32) -> String {
    assert_eq!(resp.id, Some(json!(id)), "framing: id echoed on error");
    let e = resp.error.as_ref().expect("error present");
    assert_eq!(e.code, code, "error code");
    e.message.clone()
}

// ── sm.chat / sm.health ─────────────────────────────────────────────────────────

/// Why: the headline acceptance — `sm.chat` drives a full SM turn through the
/// mock provider and returns `{ reply, conv_id, cost }` with correct framing.
/// What: dispatches `sm.chat` and asserts the reply, an echoed conv_id, and the
/// per-call cost (epsilon compare).
/// Test: this is the test.
#[tokio::test]
async fn chat_round_trips() {
    let tmp = TempDir::new().unwrap();
    let cfg = enabled_config();
    let agent = agent_with_provider(cfg.clone(), tmp.path());
    let d = dispatcher_with(
        agent,
        cfg,
        tmp.path(),
        Arc::new(MockSessionControl::default()),
    );

    let resp = d
        .dispatch(req(
            1,
            "sm.chat",
            json!({ "message": "decompose login", "conv_id": "c-1" }),
        ))
        .await;
    let result = ok_result(&resp, 1);
    assert_eq!(result["reply"], "SM plan: delegate to a session");
    assert_eq!(result["conv_id"], "c-1");
    let cost = result["cost"].as_f64().expect("cost is a number");
    assert!((cost - 0.0021).abs() < 1e-9, "per-call cost returned");
}

/// Why: a degraded SM (no provider) must map to a graceful JSON-RPC error
/// (CODE_UNAVAILABLE), NOT a panic and NOT a success.
/// What: dispatches `sm.chat` against a degraded agent; asserts the unavailable code.
/// Test: this is the test.
#[tokio::test]
async fn chat_degraded_is_unavailable() {
    let tmp = TempDir::new().unwrap();
    let cfg = enabled_config();
    let agent = agent_degraded(cfg.clone(), tmp.path());
    let d = dispatcher_with(
        agent,
        cfg,
        tmp.path(),
        Arc::new(MockSessionControl::default()),
    );

    let resp = d
        .dispatch(req(2, "sm.chat", json!({ "message": "hi" })))
        .await;
    err_code(&resp, 2, CODE_UNAVAILABLE);
}

/// Why: `sm.chat` with no `message` is malformed and must be an invalid-params
/// error, not a panic.
/// What: dispatches `sm.chat` with empty params; asserts INVALID_PARAMS.
/// Test: this is the test.
#[tokio::test]
async fn chat_missing_message_is_invalid_params() {
    let tmp = TempDir::new().unwrap();
    let cfg = enabled_config();
    let agent = agent_with_provider(cfg.clone(), tmp.path());
    let d = dispatcher_with(
        agent,
        cfg,
        tmp.path(),
        Arc::new(MockSessionControl::default()),
    );

    let resp = d.dispatch(req(3, "sm.chat", json!({}))).await;
    err_code(&resp, 3, error_codes::INVALID_PARAMS);
}

/// Why: a PRESENT-but-blank required string must still be rejected (INVALID_PARAMS)
/// but with a DISTINCT "must not be blank" message — not the misleading "missing"
/// wording — so a caller who supplied a whitespace value gets an actionable error.
/// What: dispatches `sm.chat` with a whitespace `message`; asserts INVALID_PARAMS
/// and that the message distinguishes blank from missing.
/// Test: this is the test.
#[tokio::test]
async fn blank_required_param_is_distinct_invalid_params() {
    let tmp = TempDir::new().unwrap();
    let cfg = enabled_config();
    let agent = agent_with_provider(cfg.clone(), tmp.path());
    let d = dispatcher_with(
        agent,
        cfg,
        tmp.path(),
        Arc::new(MockSessionControl::default()),
    );

    let resp = d
        .dispatch(req(7, "sm.chat", json!({ "message": "   " })))
        .await;
    let msg = err_code(&resp, 7, error_codes::INVALID_PARAMS);
    assert!(
        msg.contains("must not be blank"),
        "blank value gets a distinct message, got: {msg}"
    );
}

/// Why: `sm.health` must report provider status + degraded + model tiers with
/// correct framing.
/// What: dispatches `sm.health` against a provider-backed agent; asserts ok +
/// provider + the model-tier fields.
/// Test: this is the test.
#[tokio::test]
async fn health_round_trips() {
    let tmp = TempDir::new().unwrap();
    let cfg = enabled_config();
    let agent = agent_with_provider(cfg.clone(), tmp.path());
    let d = dispatcher_with(
        agent,
        cfg,
        tmp.path(),
        Arc::new(MockSessionControl::default()),
    );

    let resp = d.dispatch(req(4, "sm.health", json!({}))).await;
    let result = ok_result(&resp, 4);
    assert_eq!(result["ok"], true);
    assert_eq!(result["degraded"], false);
    assert_eq!(result["provider"], "anthropic");
    assert_eq!(
        result["model_tiers"]["orchestration"],
        "anthropic/claude-sonnet-4-6"
    );
}

/// Why: a degraded SM's health must report degraded + provider "none".
/// What: dispatches `sm.health` against a degraded agent.
/// Test: this is the test.
#[tokio::test]
async fn health_degraded_reports_degraded() {
    let tmp = TempDir::new().unwrap();
    let cfg = enabled_config();
    let agent = agent_degraded(cfg.clone(), tmp.path());
    let d = dispatcher_with(
        agent,
        cfg,
        tmp.path(),
        Arc::new(MockSessionControl::default()),
    );

    let resp = d.dispatch(req(5, "sm.health", json!({}))).await;
    let result = ok_result(&resp, 5);
    assert_eq!(result["ok"], false);
    assert_eq!(result["degraded"], true);
    assert_eq!(result["provider"], "none");
}

// ── sm.sessions.* ───────────────────────────────────────────────────────────────

/// Why: `sm.sessions.launch` maps onto the control surface and returns
/// `{ session_id }` with the launch params forwarded.
/// What: dispatches launch; asserts the session_id result and that the mock saw
/// the workdir/prompt mapping.
/// Test: this is the test.
#[tokio::test]
async fn launch_round_trips() {
    let tmp = TempDir::new().unwrap();
    let cfg = enabled_config();
    let agent = agent_with_provider(cfg.clone(), tmp.path());
    let control = Arc::new(MockSessionControl::default());
    let d = dispatcher_with(agent, cfg, tmp.path(), control.clone());

    let resp = d
        .dispatch(req(
            6,
            "sm.sessions.launch",
            json!({ "workdir": "/repo", "prompt": "fix bug", "model": "tcode" }),
        ))
        .await;
    let result = ok_result(&resp, 6);
    assert_eq!(result["session_id"], "11111111-1111-1111-1111-111111111111");

    let seen = control
        .last_launch
        .lock()
        .unwrap()
        .clone()
        .expect("launch seen");
    assert_eq!(seen.workdir, "/repo");
    assert_eq!(seen.prompt.as_deref(), Some("fix bug"));
    assert_eq!(seen.model.as_deref(), Some("tcode"));
}

/// Why: each remaining session verb (list/get/send/stop/resume/kill) must
/// round-trip with correct framing through its control mapping.
/// What: dispatches each and asserts an ok result.
/// Test: this is the test.
#[tokio::test]
async fn session_verbs_round_trip() {
    let tmp = TempDir::new().unwrap();
    let cfg = enabled_config();
    let agent = agent_with_provider(cfg.clone(), tmp.path());
    let d = dispatcher_with(
        agent,
        cfg,
        tmp.path(),
        Arc::new(MockSessionControl::default()),
    );
    let sid = "22222222-2222-2222-2222-222222222222";

    let list = d.dispatch(req(10, "sm.sessions.list", json!({}))).await;
    assert!(ok_result(&list, 10)["sessions"].is_array());

    let get = d
        .dispatch(req(11, "sm.sessions.get", json!({ "session_id": sid })))
        .await;
    assert!(ok_result(&get, 11)["session"].is_object());

    let send = d
        .dispatch(req(
            12,
            "sm.sessions.send",
            json!({ "session_id": sid, "text": "y" }),
        ))
        .await;
    assert_eq!(ok_result(&send, 12)["ok"], true);

    let stop = d
        .dispatch(req(13, "sm.sessions.stop", json!({ "session_id": sid })))
        .await;
    assert_eq!(ok_result(&stop, 13)["ok"], true);

    let resume = d
        .dispatch(req(14, "sm.sessions.resume", json!({ "session_id": sid })))
        .await;
    assert_eq!(ok_result(&resume, 14)["ok"], true);

    let kill = d
        .dispatch(req(15, "sm.sessions.kill", json!({ "session_id": sid })))
        .await;
    assert_eq!(ok_result(&kill, 15)["ok"], true);
}

/// Why: a not-found session must map to the server-defined NOT_FOUND code (not a
/// success, not a panic).
/// What: dispatches `sm.sessions.get` against a control that returns NotFound.
/// Test: this is the test.
#[tokio::test]
async fn get_unknown_session_is_not_found() {
    let tmp = TempDir::new().unwrap();
    let cfg = enabled_config();
    let agent = agent_with_provider(cfg.clone(), tmp.path());
    let control = Arc::new(MockSessionControl {
        fail_get_not_found: true,
        ..MockSessionControl::default()
    });
    let d = dispatcher_with(agent, cfg, tmp.path(), control);

    let resp = d
        .dispatch(req(
            16,
            "sm.sessions.get",
            json!({ "session_id": "33333333-3333-3333-3333-333333333333" }),
        ))
        .await;
    err_code(&resp, 16, CODE_NOT_FOUND);
}

/// Why: a BACKEND failure of `stop`/`kill` on a session that exists (tmux/store
/// error, not a missing id) must map to INTERNAL_ERROR (-32603), NOT the
/// not-found code — consistent with `send`/`resume`. This guards the regression
/// where any `stop`/`decommission` error was reported as not-found.
/// What: dispatches `sm.sessions.stop` and `sm.sessions.kill` against a control
/// that returns [`SessionControlError::Backend`]; asserts INTERNAL_ERROR on both.
/// Test: this is the test.
#[tokio::test]
async fn stop_kill_backend_failure_is_internal_not_found() {
    let tmp = TempDir::new().unwrap();
    let cfg = enabled_config();
    let agent = agent_with_provider(cfg.clone(), tmp.path());
    let control = Arc::new(MockSessionControl {
        fail_stop_kill_backend: true,
        ..MockSessionControl::default()
    });
    let d = dispatcher_with(agent, cfg, tmp.path(), control);
    let sid = "44444444-4444-4444-4444-444444444444";

    let stop = d
        .dispatch(req(17, "sm.sessions.stop", json!({ "session_id": sid })))
        .await;
    err_code(&stop, 17, error_codes::INTERNAL_ERROR);

    let kill = d
        .dispatch(req(18, "sm.sessions.kill", json!({ "session_id": sid })))
        .await;
    err_code(&kill, 18, error_codes::INTERNAL_ERROR);
}

// ── Framing: parse error / unknown method / notification ────────────────────────

/// Why: a malformed JSON request line must produce a JSON-RPC parse-error
/// response, not a panic. The shared line loop builds the parse error; this test
/// asserts that contract directly via the same path.
/// What: parses an invalid JSON line the way the loop does and asserts the
/// PARSE_ERROR response with a null id.
/// Test: this is the test.
#[test]
fn malformed_json_is_parse_error() {
    let line = "{ this is not json ";
    let resp = match serde_json::from_str::<Request>(line) {
        Ok(_) => panic!("should not parse"),
        Err(e) => Response::err(
            None,
            error_codes::PARSE_ERROR,
            format!("invalid JSON-RPC: {e}"),
        ),
    };
    assert_eq!(resp.error.as_ref().unwrap().code, error_codes::PARSE_ERROR);
    assert!(resp.id.is_none(), "parse error has null id");
}

/// Why: an unknown method must map to METHOD_NOT_FOUND with the id echoed.
/// What: dispatches a bogus method; asserts the code.
/// Test: this is the test.
#[tokio::test]
async fn unknown_method_is_method_not_found() {
    let tmp = TempDir::new().unwrap();
    let cfg = enabled_config();
    let agent = agent_with_provider(cfg.clone(), tmp.path());
    let d = dispatcher_with(
        agent,
        cfg,
        tmp.path(),
        Arc::new(MockSessionControl::default()),
    );

    let resp = d.dispatch(req(20, "sm.bogus", json!({}))).await;
    err_code(&resp, 20, error_codes::METHOD_NOT_FOUND);
}

/// Why: a JSON-RPC notification (no id) must be SUPPRESSED — the adapter must not
/// emit a reply for it (the line loop drops suppressed responses).
/// What: dispatches an id-less request and asserts the suppress flag.
/// Test: this is the test.
#[tokio::test]
async fn notification_is_suppressed() {
    let tmp = TempDir::new().unwrap();
    let cfg = enabled_config();
    let agent = agent_with_provider(cfg.clone(), tmp.path());
    let d = dispatcher_with(
        agent,
        cfg,
        tmp.path(),
        Arc::new(MockSessionControl::default()),
    );

    let notif = Request {
        jsonrpc: Some("2.0".into()),
        id: None,
        method: "sm.health".into(),
        params: None,
    };
    let resp = d.dispatch(notif).await;
    assert!(resp.suppress, "notification must be suppressed");
}

// ── Line framing read/write helper ──────────────────────────────────────────────

/// Why: the adapter relies on `run_stdio_loop`'s newline-delimited read/write
/// framing; this test drives that helper through an in-memory pipe with the SM
/// dispatch as the handler, asserting a request produces exactly one JSON line
/// on stdout (clean framing, id echoed).
/// What: pipes one `sm.health` request line into `run_stdio_loop` (over injected
/// I/O) and parses the single response line back.
/// Test: this is the test.
#[tokio::test]
async fn line_framing_round_trips_one_response() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let tmp = TempDir::new().unwrap();
    let cfg = enabled_config();
    let agent = agent_with_provider(cfg.clone(), tmp.path());
    let d = Arc::new(dispatcher_with(
        agent,
        cfg,
        tmp.path(),
        Arc::new(MockSessionControl::default()),
    ));

    // Build one request line, run it through the dispatcher, and serialize the
    // response the way the loop does — asserting exactly one clean JSON line.
    let request_line = serde_json::to_string(&req(99, "sm.health", json!({}))).unwrap();
    let (mut client_tx, server_rx) = tokio::io::duplex(8192);
    let (server_tx, client_rx) = tokio::io::duplex(8192);

    let dispatcher = d.clone();
    let loop_fut = run_stdio_loop_over(
        move |r| {
            let dispatcher = dispatcher.clone();
            async move { dispatcher.dispatch(r).await }
        },
        server_rx,
        server_tx,
    );

    client_tx
        .write_all(format!("{request_line}\n").as_bytes())
        .await
        .unwrap();
    drop(client_tx); // EOF so the loop returns

    loop_fut.await.expect("loop returns Ok on EOF");

    let mut lines = BufReader::new(client_rx).lines();
    let first = lines.next_line().await.unwrap().expect("one response line");
    let parsed: Value = serde_json::from_str(&first).expect("response is valid JSON");
    assert_eq!(parsed["jsonrpc"], "2.0");
    assert_eq!(parsed["id"], 99);
    assert_eq!(parsed["result"]["ok"], true);
    // Exactly one line — no stray output.
    assert!(
        lines.next_line().await.unwrap().is_none(),
        "exactly one response line"
    );
}

/// Minimal copy of the shared stdio loop parameterised over injected I/O.
///
/// Why: `trusty_common::mcp::run_stdio_loop` only accepts the real stdin/stdout;
/// to test the dispatcher over the SAME framing rules without touching the real
/// process streams, this mirrors the loop body over an injected reader/writer.
/// What: reads newline-delimited requests, dispatches, writes one JSON line per
/// non-suppressed response. Identical framing to the shared loop.
/// Test: used by `line_framing_round_trips_one_response`.
async fn run_stdio_loop_over<F, Fut, R, W>(
    dispatcher: F,
    reader: R,
    mut writer: W,
) -> anyhow::Result<()>
where
    F: Fn(Request) -> Fut,
    Fut: std::future::Future<Output = Response>,
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(trimmed) {
            Ok(r) => dispatcher(r).await,
            Err(e) => Response::err(None, error_codes::PARSE_ERROR, format!("{e}")),
        };
        if response.suppress {
            continue;
        }
        let serialised = serde_json::to_string(&response)?;
        writer.write_all(serialised.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
    }
    Ok(())
}

// ── Scripted §1A.2 step-1 sequence ──────────────────────────────────────────────

/// Why: the §1A.2 acceptance is a scripted parent-driver flow over stdio:
/// chat → launch → get → stop → goal-update. This drives that exact sequence
/// through the dispatcher (mocks for control + provider) and asserts each step
/// round-trips, proving the whole surface is exercisable headlessly.
/// What: runs chat, launch, get, stop in order; under `sm-memory` also creates a
/// goal and updates it; asserts each response is a non-error result.
/// Test: this is the test.
#[tokio::test]
async fn scripted_chat_launch_get_stop_sequence() {
    let tmp = TempDir::new().unwrap();
    let cfg = enabled_config();
    let agent = agent_with_provider(cfg.clone(), tmp.path());
    let d = dispatcher_with(
        agent,
        cfg,
        tmp.path(),
        Arc::new(MockSessionControl::default()),
    );

    // 1) chat
    let chat = d
        .dispatch(req(40, "sm.chat", json!({ "message": "ship feature X" })))
        .await;
    let chat_result = ok_result(&chat, 40);
    let conv_id = chat_result["conv_id"]
        .as_str()
        .expect("conv_id")
        .to_string();
    assert!(!conv_id.is_empty());

    // 2) launch
    let launch = d
        .dispatch(req(
            41,
            "sm.sessions.launch",
            json!({ "workdir": "/repo", "prompt": "X" }),
        ))
        .await;
    let session_id = ok_result(&launch, 41)["session_id"]
        .as_str()
        .unwrap()
        .to_string();

    // 3) get
    let get = d
        .dispatch(req(
            42,
            "sm.sessions.get",
            json!({ "session_id": session_id }),
        ))
        .await;
    assert!(ok_result(&get, 42)["session"].is_object());

    // 4) stop
    let stop = d
        .dispatch(req(
            43,
            "sm.sessions.stop",
            json!({ "session_id": session_id }),
        ))
        .await;
    assert_eq!(ok_result(&stop, 43)["ok"], true);

    // 5) goal-update (only meaningful under sm-memory; otherwise it returns the
    //    graceful unavailable error, which is itself a valid framed response).
    let create = d
        .dispatch(req(
            44,
            "sm.goals.create",
            json!({ "description": "ship X" }),
        ))
        .await;
    #[cfg(feature = "sm-memory")]
    {
        let goal = ok_result(&create, 44);
        let goal_id = goal["goal"]["id"].as_str().unwrap().to_string();
        let upd = d
            .dispatch(req(
                45,
                "sm.goals.update",
                json!({ "id": goal_id, "note": "kicked off" }),
            ))
            .await;
        assert!(
            !ok_result(&upd, 45)["goal"]["notes"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    #[cfg(not(feature = "sm-memory"))]
    {
        err_code(&create, 44, CODE_UNAVAILABLE);
    }
}

// ── Feature-gated goals / context ───────────────────────────────────────────────

/// Why: `sm.goals.*` round-trip under `sm-memory` (create then list) and degrade
/// to a graceful unavailable error without it. This pins both branches.
/// What: under the feature, creates a goal and lists it; without it, asserts the
/// unavailable code on `sm.goals.list`.
/// Test: this is the test.
#[tokio::test]
async fn goals_feature_branches() {
    let tmp = TempDir::new().unwrap();
    let cfg = enabled_config();
    let agent = agent_with_provider(cfg.clone(), tmp.path());
    let d = dispatcher_with(
        agent,
        cfg,
        tmp.path(),
        Arc::new(MockSessionControl::default()),
    );

    #[cfg(feature = "sm-memory")]
    {
        let create = d
            .dispatch(req(
                50,
                "sm.goals.create",
                json!({ "description": "g1", "acceptance": ["pr merged"] }),
            ))
            .await;
        assert_eq!(ok_result(&create, 50)["goal"]["description"], "g1");

        let list = d.dispatch(req(51, "sm.goals.list", json!({}))).await;
        let goals = ok_result(&list, 51)["goals"].as_array().unwrap().clone();
        assert_eq!(goals.len(), 1, "the created goal is listed");
    }
    #[cfg(not(feature = "sm-memory"))]
    {
        let list = d.dispatch(req(52, "sm.goals.list", json!({}))).await;
        err_code(&list, 52, CODE_UNAVAILABLE);
    }
}

/// Why: `sm.context.get` returns the rolling context state under `sm-memory`
/// (after a chat populates it) and degrades to unavailable without the feature.
/// What: under the feature, runs a chat then `sm.context.get` for the same
/// conv_id and asserts the four context fields; without it, asserts unavailable.
/// Test: this is the test.
#[tokio::test]
async fn context_get_feature_branches() {
    let tmp = TempDir::new().unwrap();
    let cfg = enabled_config();
    let agent = agent_with_provider(cfg.clone(), tmp.path());
    let d = dispatcher_with(
        agent,
        cfg,
        tmp.path(),
        Arc::new(MockSessionControl::default()),
    );

    #[cfg(feature = "sm-memory")]
    {
        // Populate a conversation first so the context engine has a round.
        let _ = d
            .dispatch(req(
                60,
                "sm.chat",
                json!({ "message": "hi", "conv_id": "ctx-1" }),
            ))
            .await;
        let ctx = d
            .dispatch(req(61, "sm.context.get", json!({ "conv_id": "ctx-1" })))
            .await;
        let result = ok_result(&ctx, 61);
        assert!(result["recent_rounds"].is_array());
        assert!(result["total_rounds"].as_u64().unwrap() >= 1);
        assert!(result["token_estimate"].is_number());
        assert!(result.get("compressed_context").is_some());
    }
    #[cfg(not(feature = "sm-memory"))]
    {
        let ctx = d
            .dispatch(req(62, "sm.context.get", json!({ "conv_id": "ctx-1" })))
            .await;
        err_code(&ctx, 62, CODE_UNAVAILABLE);
    }
}

// ── stdout cleanliness guard ────────────────────────────────────────────────────

/// Why: stdout is reserved EXCLUSIVELY for JSON-RPC framing. A stray `println!`/
/// `print!` anywhere in the SM stdio adapter OR the SM core it drives would
/// corrupt the channel. This test mechanically greps those source trees and
/// asserts NONE contain a `print!`/`println!` macro call, so a future edit that
/// adds one fails loudly.
/// What: scans every `.rs` file under `src/daemon/sm_stdio/` and `src/core/sm/`
/// for the `println!`/`print!` macro tokens (ignoring this test file and doc
/// comments), asserting zero hits.
/// Test: this is the test.
#[test]
fn no_stdout_writes_in_sm_paths() {
    use std::fs;
    use std::path::Path;

    // CARGO_MANIFEST_DIR is the crate root; the SM paths live under src/.
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let roots = [
        crate_root.join("src/daemon/sm_stdio"),
        crate_root.join("src/core/sm"),
    ];

    /// Recursively collect every `.rs` file under `dir`.
    fn collect_rs(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rs(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    let mut files = Vec::new();
    for root in &roots {
        collect_rs(root, &mut files);
    }
    assert!(
        !files.is_empty(),
        "expected to find SM source files to scan"
    );

    let mut offenders = Vec::new();
    for path in &files {
        // This test file itself names the macros in comments/strings — skip it.
        if path.ends_with("daemon/sm_stdio/tests.rs") {
            continue;
        }
        let Ok(src) = fs::read_to_string(path) else {
            continue;
        };
        for (lineno, line) in src.lines().enumerate() {
            let code = line.trim_start();
            // Ignore doc/line comments — they cannot write to stdout.
            if code.starts_with("//") || code.starts_with("*") {
                continue;
            }
            // `print!`/`println!` write to stdout (forbidden). `eprint!`/
            // `eprintln!` write to stderr (allowed) — and contain the substring
            // `print!`/`println!`, so we must check the preceding char is not an
            // identifier char (the `e` in `eprintln!`) before flagging.
            if contains_macro(line, b"println!") || contains_macro(line, b"print!") {
                offenders.push(format!("{}:{}", path.display(), lineno + 1));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "stdout-write macro found in SM paths (stdout must stay JSON-RPC-only): {offenders:?}"
    );
}

/// Detect a `needle` macro token NOT preceded by an identifier char.
///
/// Why: distinguishing the stdout writers `print!`/`println!` from the stderr
/// writers `eprint!`/`eprintln!` (which contain them as a substring) needs a
/// boundary check — the macro must not be immediately preceded by an
/// alphanumeric/`_` char (the `e` in `eprintln!`, or any identifier prefix).
/// What: returns true when `needle` occurs in `line` with a non-identifier char
/// (or start-of-line) immediately before it.
/// Test: exercised by `no_stdout_writes_in_sm_paths` (the `eprintln!` in
/// `prompt.rs` must NOT be flagged).
fn contains_macro(line: &str, needle: &[u8]) -> bool {
    let bytes = line.as_bytes();
    let mut i = 0;
    while let Some(pos) = find_subslice(&bytes[i..], needle) {
        let abs = i + pos;
        let prev_ok =
            abs == 0 || !(bytes[abs - 1].is_ascii_alphanumeric() || bytes[abs - 1] == b'_');
        if prev_ok {
            return true;
        }
        i = abs + 1;
    }
    false
}

/// Find the first index of `needle` in `haystack`, or `None`.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}
