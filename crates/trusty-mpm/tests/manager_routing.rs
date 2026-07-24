//! Hermetic HTTP integration suite for the Layer-3 manager PHASE 2 routing +
//! proposal-and-confirm surface (`/api/v1/manager/{route-task,act,chat}`, epic
//! #2109, DOC-36 phase 2: WI-8 #2585, WI-9 #2586).
//!
//! Why: DOC-36 §4's local-testability bar requires these endpoints to be
//! exercisable with NO live provider key, NO network, and NO channel/bot token.
//! This file binds the REAL `api::router` on a loopback port and drives:
//! (1) `route-task` — the unambiguous deterministic pass-through (no LLM), the
//! LLM-judged disambiguation on a genuine tie (via `ScriptedAdapter`), the
//! no-provider degrade to the deterministic top candidate, no-match, and the
//! empty-text 400; (2) `act` — the propose→confirm protocol, proving a call
//! without `confirm` executes NOTHING and a `confirm: true` call routes a launch
//! through a test-double `SessionLauncher` and an inject through a REAL
//! `SessionProxy` over a test-double `ManagedBackend` (no live session/channel);
//! (3) `chat`'s IN-CONVERSATION propose→confirm action flow (coordinator review
//! of #2586's primary acceptance criterion) — a proposal turn executes nothing,
//! the immediately-following confirm turn on the SAME conversation_key executes
//! exactly once through the SAME actuator seam `act` uses, confirming on a
//! DIFFERENT conversation_key never executes, an unconfirmed proposal expires
//! after exactly one intervening turn (next-turn-only TTL), and a plain message
//! never triggers execution.
//! What: this file IS the test; run with
//! `cargo test -p trusty-mpm --test manager_routing`.

use std::future::IntoFuture;
use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{Value, json};
use trusty_common::inference::registry::{ProviderId, capabilities};
use trusty_common::inference::test_support::ScriptedAdapter;
use trusty_common::inference::types::UsageBlock;
use trusty_common::inference::{AssistantMessage, ChatChoice, ChatResponse, InferenceAdapter};
use trusty_mpm::client::proxy::{ActivityDigest, ManagedBackend, SessionProxy};
use trusty_mpm::daemon::manager::{LaunchOutcome, ProxyActuator, SessionLauncher};
use trusty_mpm::daemon::{api, state::DaemonState};
use trusty_mpm::project::Project;

/// Serve the real router on an ephemeral loopback port; return its base URL.
async fn serve(state: Arc<DaemonState>) -> String {
    let router = api::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, router).into_future());
    format!("http://{addr}")
}

/// A fresh, hermetic daemon state under a temp framework root.
async fn fresh_state() -> Arc<DaemonState> {
    let root = tempfile::tempdir().unwrap().keep();
    Arc::new(DaemonState::with_root_isolated_managed(root).await)
}

/// Register a project fixture with the given name and tags.
async fn register_project(state: &Arc<DaemonState>, name: &str, tags: &[&str]) {
    state
        .project_registry()
        .await
        .register(Project {
            name: name.to_string(),
            repo_url: format!("https://github.com/acme/{name}"),
            default_branch: "main".to_string(),
            stack_hint: None,
            tags: tags.iter().map(|t| t.to_string()).collect(),
            description: None,
            gh_user: None,
            gh_account: None,
            github: None,
            commit_name: None,
            commit_email: None,
            worktree: None,
        })
        .await
        .expect("register project");
}

/// Build a scripted OpenAI-shaped response carrying `text`.
fn scripted_reply(text: &str) -> ChatResponse {
    ChatResponse {
        id: "scripted".into(),
        model: "test/model".into(),
        choices: vec![ChatChoice {
            message: AssistantMessage {
                content: Some(text.into()),
                tool_calls: Vec::new(),
            },
            finish_reason: Some("stop".into()),
        }],
        usage: UsageBlock::default(),
    }
}

/// Install a scripted adapter into the served state's manager inference seam.
fn install_scripted(state: &Arc<DaemonState>, adapter: ScriptedAdapter) {
    let adapter: Arc<dyn InferenceAdapter> = Arc::new(adapter);
    state
        .manager_state()
        .inference()
        .set_adapter(adapter, "test/model");
}

/// POST helper returning `(status, body_json)`.
async fn post(base: &str, path: &str, body: Value) -> (reqwest::StatusCode, Value) {
    let resp = reqwest::Client::new()
        .post(format!("{base}{path}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let json: Value = resp.json().await.unwrap();
    (status, json)
}

// ── route-task ──────────────────────────────────────────────────────────────

/// Why: an unambiguous exact-name match must pass straight through the
/// deterministic resolver with NO inference call (resolved_by = "resolver").
/// Test: itself.
#[tokio::test]
async fn route_task_unambiguous_pass_through_no_llm() {
    let state = fresh_state().await;
    register_project(&state, "alpha", &[]).await;
    // No inference provider configured — proving the unambiguous path never
    // touches inference (it would otherwise degrade/err).
    state.manager_state().inference().set_unconfigured();
    let base = serve(Arc::clone(&state)).await;

    let (status, body) = post(
        &base,
        "/api/v1/manager/route-task",
        json!({ "text": "alpha" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["project"], "alpha");
    assert_eq!(body["resolved_by"], "resolver");
    assert!((body["confidence"].as_f64().unwrap() - 1.0).abs() < 1e-6);
}

/// Why: a genuine tie (two candidates above the disambiguation floor) must
/// escalate to ONE LLM judgment call, and the judged pick is returned with
/// resolved_by = "disambiguation".
/// Test: itself.
#[tokio::test]
async fn route_task_tie_escalates_to_llm_disambiguation() {
    let state = fresh_state().await;
    // Both score name(0.4)+tag(0.3)=0.7 for the query "auth" → both > 0.6 floor.
    register_project(&state, "auth-alpha", &["auth"]).await;
    register_project(&state, "auth-beta", &["auth"]).await;
    install_scripted(
        &state,
        ScriptedAdapter::new("scripted", capabilities(ProviderId::OpenRouter))
            .with_response(scripted_reply("auth-beta")),
    );
    let base = serve(Arc::clone(&state)).await;

    let (status, body) = post(
        &base,
        "/api/v1/manager/route-task",
        json!({ "text": "auth" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["resolved_by"], "disambiguation", "body: {body}");
    // The LLM picked auth-beta even though auth-alpha is the deterministic primary.
    assert_eq!(body["project"], "auth-beta", "body: {body}");
}

/// Why: on a tie with NO inference provider, route-task must degrade to the
/// deterministic top candidate rather than fail (advisory, never panics).
/// Test: itself.
#[tokio::test]
async fn route_task_tie_degrades_without_provider() {
    let state = fresh_state().await;
    register_project(&state, "auth-alpha", &["auth"]).await;
    register_project(&state, "auth-beta", &["auth"]).await;
    state.manager_state().inference().set_unconfigured();
    let base = serve(Arc::clone(&state)).await;

    let (status, body) = post(
        &base,
        "/api/v1/manager/route-task",
        json!({ "text": "auth" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    // Degraded to the deterministic pick — resolver, not disambiguation.
    assert_eq!(body["resolved_by"], "resolver", "body: {body}");
    assert!(body["project"].is_string(), "body: {body}");
    assert!(
        body["rationale"]
            .as_str()
            .unwrap()
            .contains("no inference provider"),
        "body: {body}"
    );
}

/// Why: an unresolvable query is an advisory 200 no-match (project = null), not
/// an error — the caller decides what to do next.
/// Test: itself.
#[tokio::test]
async fn route_task_no_match_is_advisory() {
    let state = fresh_state().await;
    register_project(&state, "alpha", &[]).await;
    let base = serve(Arc::clone(&state)).await;

    let (status, body) = post(
        &base,
        "/api/v1/manager/route-task",
        json!({ "text": "zzzz-nothing-matches" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(body["project"].is_null(), "body: {body}");
    assert_eq!(body["resolved_by"], "no_match");
}

/// Why: an empty task is a client error (400), not a silent no-match.
/// Test: itself.
#[tokio::test]
async fn route_task_empty_text_is_400() {
    let state = fresh_state().await;
    let base = serve(Arc::clone(&state)).await;
    let (status, _body) = post(&base, "/api/v1/manager/route-task", json!({ "text": "  " })).await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
}

// ── act: propose-and-confirm ─────────────────────────────────────────────────

/// A recording test double for the launch verb — records the launch and returns
/// a canned session WITHOUT provisioning anything.
struct FakeLauncher {
    launched: Mutex<Vec<(String, String)>>,
}

impl FakeLauncher {
    fn new() -> Self {
        Self {
            launched: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl SessionLauncher for FakeLauncher {
    async fn launch(&self, project: &str, task: &str) -> Result<LaunchOutcome, String> {
        self.launched
            .lock()
            .unwrap()
            .push((project.to_string(), task.to_string()));
        Ok(LaunchOutcome {
            session_id: "sess-launched-1".to_string(),
            name: format!("{project}-session"),
            state: "active".to_string(),
        })
    }
}

/// A test-double `ManagedBackend` — the seam the REAL `SessionProxy` drives, so
/// the confirmed inject exercises the genuine focus→inject state machine with no
/// live session.
struct FakeBackend {
    sent: Mutex<Vec<(String, String)>>,
}

impl FakeBackend {
    fn new() -> Self {
        Self {
            sent: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl ManagedBackend for FakeBackend {
    async fn resolve(&self, target: &str) -> Result<(String, String), String> {
        if target == "sess-1" || target == "id-sess-1" {
            Ok(("id-sess-1".to_string(), "sess-1".to_string()))
        } else {
            Err(format!("managed session {target} not found"))
        }
    }
    async fn send(&self, id: &str, text: &str) -> Result<(), String> {
        self.sent
            .lock()
            .unwrap()
            .push((id.to_string(), text.to_string()));
        Ok(())
    }
    async fn activity(&self, _id: &str) -> Result<ActivityDigest, String> {
        Ok(ActivityDigest {
            state: "active".to_string(),
            summary: "working on the task".to_string(),
            pending_decision: None,
        })
    }
}

/// Install a `ProxyActuator` over the two doubles onto the manager state.
fn install_actuator(
    state: &Arc<DaemonState>,
    launcher: Arc<FakeLauncher>,
    backend: Arc<FakeBackend>,
) {
    let proxy = SessionProxy::new(backend);
    let actuator = ProxyActuator::new(launcher, proxy);
    state.manager_state().set_actuator(Arc::new(actuator));
}

/// Why: a call WITHOUT `confirm` must return a proposal and execute NOTHING —
/// DOC-35 §11's "no acting without an explicit call".
/// Test: itself.
#[tokio::test]
async fn act_propose_only_executes_nothing() {
    let state = fresh_state().await;
    let launcher = Arc::new(FakeLauncher::new());
    let backend = Arc::new(FakeBackend::new());
    install_actuator(&state, Arc::clone(&launcher), Arc::clone(&backend));
    let base = serve(Arc::clone(&state)).await;

    let (status, body) = post(
        &base,
        "/api/v1/manager/act",
        json!({
            "conversation_key": "cli:test",
            "action": { "type": "launch", "project": "alpha", "task": "do the thing" }
        }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["status"], "proposed", "body: {body}");
    assert!(
        body["proposal"].as_str().unwrap().contains("confirm"),
        "body: {body}"
    );
    // Nothing was launched.
    assert!(launcher.launched.lock().unwrap().is_empty());
}

/// Why: a confirmed launch must call the #2108 launch verb (here the recording
/// double) exactly once and report the launched session.
/// Test: itself.
#[tokio::test]
async fn act_confirmed_launch_calls_launch_verb() {
    let state = fresh_state().await;
    let launcher = Arc::new(FakeLauncher::new());
    let backend = Arc::new(FakeBackend::new());
    install_actuator(&state, Arc::clone(&launcher), Arc::clone(&backend));
    let base = serve(Arc::clone(&state)).await;

    let (status, body) = post(
        &base,
        "/api/v1/manager/act",
        json!({
            "conversation_key": "cli:test",
            "confirm": true,
            "action": { "type": "launch", "project": "alpha", "task": "do the thing" }
        }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["status"], "launched", "body: {body}");
    assert_eq!(body["session_id"], "sess-launched-1");
    let launched = launcher.launched.lock().unwrap();
    assert_eq!(launched.len(), 1);
    assert_eq!(
        launched[0],
        ("alpha".to_string(), "do the thing".to_string())
    );
}

/// Why: a confirmed inject must route through the REAL `SessionProxy`
/// (focus→inject) over the test-double backend — never a direct tmux mutation.
/// Test: itself.
#[tokio::test]
async fn act_confirmed_inject_routes_through_session_proxy() {
    let state = fresh_state().await;
    let launcher = Arc::new(FakeLauncher::new());
    let backend = Arc::new(FakeBackend::new());
    install_actuator(&state, Arc::clone(&launcher), Arc::clone(&backend));
    let base = serve(Arc::clone(&state)).await;

    let (status, body) = post(
        &base,
        "/api/v1/manager/act",
        json!({
            "conversation_key": "cli:test",
            "confirm": true,
            "action": { "type": "inject", "session": "sess-1", "text": "run the tests" }
        }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["status"], "injected", "body: {body}");
    assert_eq!(body["session_id"], "id-sess-1");
    let sent = backend.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0],
        ("id-sess-1".to_string(), "run the tests".to_string())
    );
}

/// Why: injecting into an unresolvable session is an advisory 200 outcome
/// (session_not_found), and nothing is sent.
/// Test: itself.
#[tokio::test]
async fn act_confirmed_inject_unknown_session_not_found() {
    let state = fresh_state().await;
    let launcher = Arc::new(FakeLauncher::new());
    let backend = Arc::new(FakeBackend::new());
    install_actuator(&state, Arc::clone(&launcher), Arc::clone(&backend));
    let base = serve(Arc::clone(&state)).await;

    let (status, body) = post(
        &base,
        "/api/v1/manager/act",
        json!({
            "conversation_key": "cli:test",
            "confirm": true,
            "action": { "type": "inject", "session": "ghost", "text": "hi" }
        }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["status"], "session_not_found", "body: {body}");
    assert!(backend.sent.lock().unwrap().is_empty());
}

/// Why: a confirmed summarize routes through the REAL `SessionProxy` summarize
/// direction over the double.
/// Test: itself.
#[tokio::test]
async fn act_confirmed_summarize_routes_through_session_proxy() {
    let state = fresh_state().await;
    let launcher = Arc::new(FakeLauncher::new());
    let backend = Arc::new(FakeBackend::new());
    install_actuator(&state, Arc::clone(&launcher), Arc::clone(&backend));
    let base = serve(Arc::clone(&state)).await;

    let (status, body) = post(
        &base,
        "/api/v1/manager/act",
        json!({
            "conversation_key": "cli:test",
            "confirm": true,
            "action": { "type": "summarize", "session": "sess-1" }
        }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["status"], "summarized", "body: {body}");
    assert_eq!(body["summary"], "working on the task");
}

/// Why: an empty conversation key is a client error (400).
/// Test: itself.
#[tokio::test]
async fn act_empty_conversation_key_is_400() {
    let state = fresh_state().await;
    let base = serve(Arc::clone(&state)).await;
    let (status, _body) = post(
        &base,
        "/api/v1/manager/act",
        json!({
            "conversation_key": "  ",
            "action": { "type": "summarize", "session": "x" }
        }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
}

/// Why: `conversation_key` is required UNIFORMLY across every action type — a
/// deliberate consistency/audit-trail choice (coordinator review finding 3), not
/// an oversight limited to Inject/Summarize. `Launch` does not read the key
/// operationally, but an empty key must still 400, proving the rule applies to
/// (and is tested for) the one variant that never uses it.
/// Test: itself.
#[tokio::test]
async fn act_launch_also_requires_conversation_key() {
    let state = fresh_state().await;
    let launcher = Arc::new(FakeLauncher::new());
    let backend = Arc::new(FakeBackend::new());
    install_actuator(&state, Arc::clone(&launcher), Arc::clone(&backend));
    let base = serve(Arc::clone(&state)).await;

    let (status, _body) = post(
        &base,
        "/api/v1/manager/act",
        json!({
            "conversation_key": "  ",
            "confirm": true,
            "action": { "type": "launch", "project": "alpha", "task": "do the thing" }
        }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    // The launch verb was never reached.
    assert!(launcher.launched.lock().unwrap().is_empty());
}

// ── chat: in-conversation propose→confirm (coordinator review finding 1, #2586) ─

/// Install a scripted adapter with MULTIPLE queued replies (FIFO), so a test can
/// script exactly how many LLM calls it expects across several chat turns.
fn install_scripted_queue(state: &Arc<DaemonState>, replies: &[&str]) {
    let mut adapter = ScriptedAdapter::new("scripted", capabilities(ProviderId::OpenRouter));
    for reply in replies {
        adapter = adapter.with_response(scripted_reply(reply));
    }
    let adapter: Arc<dyn InferenceAdapter> = Arc::new(adapter);
    state
        .manager_state()
        .inference()
        .set_adapter(adapter, "test/model");
}

/// A reply text embedding a `manager-action` launch proposal, as the documented
/// chat system prompt instructs the model to produce.
fn proposal_reply(project: &str, task: &str) -> String {
    format!(
        "Sure, I can help with that.\n\n```manager-action\n\
         {{\"type\":\"launch\",\"project\":\"{project}\",\"task\":\"{task}\"}}\n\
         ```"
    )
}

/// Why: a turn whose LLM reply embeds a proposal must NOT execute anything — the
/// primary #2586 acceptance criterion, "propose in-conversation, execute only on
/// explicit confirmation".
/// Test: itself.
#[tokio::test]
async fn chat_proposal_turn_executes_nothing() {
    let state = fresh_state().await;
    let launcher = Arc::new(FakeLauncher::new());
    let backend = Arc::new(FakeBackend::new());
    install_actuator(&state, Arc::clone(&launcher), Arc::clone(&backend));
    let reply = proposal_reply("alpha", "fix the flaky auth test");
    install_scripted_queue(&state, &[reply.as_str()]);
    let base = serve(Arc::clone(&state)).await;

    let (status, body) = post(
        &base,
        "/api/v1/manager/chat",
        json!({ "conversation_key": "conv-1", "message": "launch a session for alpha to fix the flaky auth test" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(body["action_result"]["status"], "proposed", "body: {body}");
    assert!(
        body["reply"].as_str().unwrap().contains("confirm"),
        "body: {body}"
    );
    assert!(launcher.launched.lock().unwrap().is_empty());
}

/// Why: the VERY NEXT turn on the SAME conversation_key, confirming, must execute
/// the pending proposal EXACTLY ONCE, through the actuator seam — with ZERO
/// additional LLM calls (only 1 scripted reply is queued, for the propose turn).
/// Test: itself.
#[tokio::test]
async fn chat_confirm_turn_executes_exactly_once() {
    let state = fresh_state().await;
    let launcher = Arc::new(FakeLauncher::new());
    let backend = Arc::new(FakeBackend::new());
    install_actuator(&state, Arc::clone(&launcher), Arc::clone(&backend));
    // Only ONE reply queued — the confirm turn must consume ZERO LLM calls.
    let reply = proposal_reply("alpha", "fix the flaky auth test");
    install_scripted_queue(&state, &[reply.as_str()]);
    let base = serve(Arc::clone(&state)).await;

    let (propose_status, _propose_body) = post(
        &base,
        "/api/v1/manager/chat",
        json!({ "conversation_key": "conv-2", "message": "launch a session for alpha to fix the flaky auth test" }),
    )
    .await;
    assert_eq!(propose_status, reqwest::StatusCode::OK);

    let (confirm_status, confirm_body) = post(
        &base,
        "/api/v1/manager/chat",
        json!({ "conversation_key": "conv-2", "message": "confirm" }),
    )
    .await;
    assert_eq!(confirm_status, reqwest::StatusCode::OK);
    assert_eq!(
        confirm_body["action_result"]["status"], "launched",
        "body: {confirm_body}"
    );
    let launched = launcher.launched.lock().unwrap();
    assert_eq!(launched.len(), 1, "must execute exactly once");
    assert_eq!(
        launched[0],
        ("alpha".to_string(), "fix the flaky auth test".to_string())
    );
}

/// Why: confirming on a DIFFERENT conversation_key than the one that proposed
/// must NOT execute anything — proposals are strictly conversation-scoped.
/// Test: itself.
#[tokio::test]
async fn chat_confirm_on_different_conversation_key_does_not_execute() {
    let state = fresh_state().await;
    let launcher = Arc::new(FakeLauncher::new());
    let backend = Arc::new(FakeBackend::new());
    install_actuator(&state, Arc::clone(&launcher), Arc::clone(&backend));
    // Propose turn (key A) consumes 1 reply; the "confirm" on key B has no
    // pending proposal so it falls through to a normal (2nd scripted) LLM call.
    let reply = proposal_reply("alpha", "fix the flaky auth test");
    install_scripted_queue(&state, &[reply.as_str(), "I'm not sure what you mean."]);
    let base = serve(Arc::clone(&state)).await;

    post(
        &base,
        "/api/v1/manager/chat",
        json!({ "conversation_key": "conv-a", "message": "launch a session for alpha to fix the flaky auth test" }),
    )
    .await;

    let (status, body) = post(
        &base,
        "/api/v1/manager/chat",
        json!({ "conversation_key": "conv-b", "message": "confirm" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    // No pending proposal on conv-b, so this was treated as an ordinary message
    // — no action_result, and definitely nothing launched.
    assert!(body.get("action_result").is_none(), "body: {body}");
    assert!(launcher.launched.lock().unwrap().is_empty());
}

/// Why: an UNCONFIRMED proposal must expire after exactly one intervening turn —
/// a later "confirm" (a THIRD turn) must find nothing pending and therefore not
/// execute (the next-turn-only TTL policy).
/// Test: itself.
#[tokio::test]
async fn chat_unconfirmed_proposal_expires_next_turn() {
    let state = fresh_state().await;
    let launcher = Arc::new(FakeLauncher::new());
    let backend = Arc::new(FakeBackend::new());
    install_actuator(&state, Arc::clone(&launcher), Arc::clone(&backend));
    // Turn 1 (propose) + turn 2 (plain, unrelated — expires the proposal) + turn
    // 3 ("confirm", but nothing pending — falls through to a normal LLM call).
    let reply = proposal_reply("alpha", "fix the flaky auth test");
    install_scripted_queue(
        &state,
        &[
            reply.as_str(),
            "Sure, happy to chat about something else.",
            "Not sure what you'd like me to confirm.",
        ],
    );
    let base = serve(Arc::clone(&state)).await;

    post(
        &base,
        "/api/v1/manager/chat",
        json!({ "conversation_key": "conv-3", "message": "launch a session for alpha to fix the flaky auth test" }),
    )
    .await;
    // Turn 2: an unrelated plain message — the pending proposal expires.
    post(
        &base,
        "/api/v1/manager/chat",
        json!({ "conversation_key": "conv-3", "message": "what else is going on?" }),
    )
    .await;
    // Turn 3: "confirm" arrives too late — nothing is pending anymore.
    let (status, body) = post(
        &base,
        "/api/v1/manager/chat",
        json!({ "conversation_key": "conv-3", "message": "confirm" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(body.get("action_result").is_none(), "body: {body}");
    assert!(launcher.launched.lock().unwrap().is_empty());
}

/// Why: a plain chat message (no proposal in the reply, not a confirmation)
/// must never trigger execution — the baseline "still safe by default" case.
/// Test: itself.
#[tokio::test]
async fn chat_plain_message_never_triggers_execution() {
    let state = fresh_state().await;
    let launcher = Arc::new(FakeLauncher::new());
    let backend = Arc::new(FakeBackend::new());
    install_actuator(&state, Arc::clone(&launcher), Arc::clone(&backend));
    install_scripted_queue(&state, &["Everything looks fine, nothing blocked."]);
    let base = serve(Arc::clone(&state)).await;

    let (status, body) = post(
        &base,
        "/api/v1/manager/chat",
        json!({ "conversation_key": "conv-4", "message": "what needs my attention?" }),
    )
    .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(body.get("action_result").is_none(), "body: {body}");
    assert_eq!(body["reply"], "Everything looks fine, nothing blocked.");
    assert!(launcher.launched.lock().unwrap().is_empty());
}
