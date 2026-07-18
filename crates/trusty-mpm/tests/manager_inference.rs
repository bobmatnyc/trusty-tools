//! Hermetic HTTP integration suite for the Layer-3 manager digest + chat surface
//! (`/api/v1/manager/{digest,chat}`, epic #2109, DOC-36 phase 1: WI-3 #2580,
//! WI-4 #2581, WI-7 #2584).
//!
//! Why: DOC-36 §4's local-testability bar requires the inference-backed manager
//! endpoints to be exercisable with NO live provider key, NO network, and NO
//! channel/bot token. This file binds the REAL `api::router` on a loopback port
//! and drives digest/chat with `reqwest`, wiring the manager's inference seam to
//! `trusty_common::inference::test_support::ScriptedAdapter` (deterministic
//! in-memory) or an `OpenAiCompatAdapter` pointed at `MockInferenceServer` (real
//! HTTP client mechanics, loopback mock). It proves: the digest happy path, the
//! no-provider deterministic-fallback degrade, project scoping, chat multi-turn
//! continuity, the no-tool-calling-surface + no-mutation-on-a-plain-message
//! invariant (chat is NO LONGER structurally read-only as of phase 2's
//! propose→confirm action flow, #2586 — see `tests/manager_routing.rs` for that
//! suite; THIS file only proves a plain reply with no confirmed proposal still
//! mutates nothing), and history threading over real HTTP.
//! What: this file IS the test; run with
//! `cargo test -p trusty-mpm --test manager_inference`.

use std::future::IntoFuture;
use std::sync::Arc;

use serde_json::{Value, json};
use trusty_common::inference::registry::{ProviderId, capabilities};
use trusty_common::inference::test_support::{MockInferenceServer, ScriptedAdapter};
use trusty_common::inference::types::UsageBlock;
use trusty_common::inference::{
    AssistantMessage, ChatChoice, ChatResponse, InferenceAdapter, OpenAiCompatAdapter,
    OpenAiCompatConfig, SecretString,
};
use trusty_mpm::daemon::{api, state::DaemonState};
use trusty_mpm::deliverable::{
    Deliverable, DeliverableId, DeliverableKind, DeliverableStatus, EstimationTier,
};
use trusty_mpm::project::Project;
use trusty_mpm::runtime::RuntimeKind;
use trusty_mpm::session_manager::ManagedSessionId;

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

/// Register a project fixture on the given daemon state.
async fn register_project(state: &Arc<DaemonState>, name: &str, repo_url: &str) {
    state
        .project_registry()
        .await
        .register(Project {
            name: name.to_string(),
            repo_url: repo_url.to_string(),
            default_branch: "main".to_string(),
            stack_hint: None,
            tags: vec![],
            description: None,
            gh_user: None,
            gh_account: None,
            github: None,
            commit_name: None,
            commit_email: None,
        })
        .await
        .expect("register project");
}

/// Seed a managed session bound to `repo_url` (starts in `Provisioning`).
async fn seed_session(state: &Arc<DaemonState>, repo_url: &str, task: &str) {
    let id = ManagedSessionId::new();
    state
        .session_manager()
        .await
        .create_with_id(
            id,
            task.to_string(),
            None,
            None,
            None,
            Some(repo_url.to_string()),
            Some("main".to_string()),
            RuntimeKind::default(),
            false,
            false,
        )
        .await
        .expect("seed session");
}

/// Seed a Deliverable scoped to `project_name` with the given status.
async fn seed_deliverable(state: &Arc<DaemonState>, project_name: &str, status: DeliverableStatus) {
    let d = Deliverable {
        id: DeliverableId::new(),
        project_name: project_name.to_string(),
        name: "fixture".to_string(),
        description: String::new(),
        kind: DeliverableKind::Feature,
        ticket_ref: None,
        spec_ref: None,
        status,
        estimated_effort: EstimationTier::M,
        created_at: chrono::Utc::now(),
        target_date: None,
    };
    state
        .deliverable_manager()
        .await
        .upsert_deliverable(d)
        .await
        .expect("seed deliverable");
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

/// Build an `OpenAiCompatAdapter` pointed at a `MockInferenceServer`.
fn mock_adapter(server: &MockInferenceServer) -> Arc<dyn InferenceAdapter> {
    let cfg = OpenAiCompatConfig {
        name: "mock".into(),
        base_url: server.url().to_string(),
        api_key: SecretString::new("test-key"), // pragma: allowlist secret
        extra_headers: Vec::new(),
        capabilities: *capabilities(ProviderId::OpenAI),
    };
    Arc::new(OpenAiCompatAdapter::new(cfg).expect("build mock adapter"))
}

/// A canned OpenAI-shaped chat body the mock server returns for every request.
fn mock_body(content: &str) -> Value {
    json!({
        "id": "mock-1",
        "model": "openai/gpt-4o-mini",
        "choices": [{
            "message": {"role": "assistant", "content": content},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8}
    })
}

// ── Digest ─────────────────────────────────────────────────────────────────────

/// Why: the digest happy path must return the model-authored narrative (marked
/// `generated_by = "llm"`) alongside the deterministic totals it was derived from.
/// Test: itself.
#[tokio::test]
async fn manager_digest_happy_path_scripted_narrative() {
    let state = fresh_state().await;
    register_project(&state, "alpha", "https://github.com/acme/alpha").await;
    seed_session(&state, "https://github.com/acme/alpha", "a-1").await;
    install_scripted(
        &state,
        ScriptedAdapter::new("scripted", capabilities(ProviderId::OpenRouter)).with_response(
            scripted_reply("Alpha has one session provisioning; nothing blocked."),
        ),
    );
    let base = serve(Arc::clone(&state)).await;

    let resp = reqwest::Client::new()
        .get(format!("{base}/api/v1/manager/digest?scope=portfolio"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["generated_by"], "llm");
    assert_eq!(body["model"], "test/model");
    assert_eq!(
        body["narrative"],
        "Alpha has one session provisioning; nothing blocked."
    );
    // Deterministic totals travel WITH the narrative — consumers never depend on
    // the model for numbers.
    assert_eq!(body["status"]["project_count"], 1);
    assert_eq!(body["status"]["totals"]["sessions"]["provisioning"], 1);
}

/// Why: with no provider configured the digest must degrade to a clearly-marked
/// deterministic templated narrative + a typed 503 — never a panic — while still
/// returning the numbers (DOC-16 D1 / §4 degrade bar).
/// Test: itself.
#[tokio::test]
async fn manager_digest_degrades_without_provider() {
    let state = fresh_state().await;
    register_project(&state, "alpha", "https://github.com/acme/alpha").await;
    seed_deliverable(&state, "alpha", DeliverableStatus::Blocked).await;
    // Force the no-provider state independent of ambient credentials/env.
    state.manager_state().inference().set_unconfigured();
    let base = serve(Arc::clone(&state)).await;

    let resp = reqwest::Client::new()
        .get(format!("{base}/api/v1/manager/digest"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["generated_by"], "deterministic_fallback");
    assert_eq!(body["error"], "inference_unavailable");
    assert!(
        body["narrative"]
            .as_str()
            .unwrap()
            .contains("deterministic fallback"),
        "fallback must be clearly marked: {body}"
    );
    assert!(body["message"].as_str().unwrap().contains("tm config keys"));
    // The deterministic rollup is still present as the fallback source of truth.
    assert_eq!(body["status"]["totals"]["deliverables"]["blocked"], 1);
}

/// Why: `scope=project:<name>` narrows the snapshot to one project (reusing the
/// per-project rollup verbatim), and an unknown project is a 404.
/// Test: itself.
#[tokio::test]
async fn manager_digest_project_scope_and_unknown_project() {
    let state = fresh_state().await;
    register_project(&state, "alpha", "https://github.com/acme/alpha").await;
    register_project(&state, "beta", "https://github.com/acme/beta").await;
    seed_session(&state, "https://github.com/acme/beta", "b-1").await;
    install_scripted(
        &state,
        ScriptedAdapter::new("scripted", capabilities(ProviderId::OpenRouter))
            .with_response(scripted_reply("Beta scope narrative.")),
    );
    let base = serve(Arc::clone(&state)).await;

    let resp = reqwest::Client::new()
        .get(format!("{base}/api/v1/manager/digest?scope=project:beta"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["scope"], "project:beta");
    assert_eq!(body["status"]["project_count"], 1);
    assert_eq!(body["status"]["projects"][0]["project_name"], "beta");

    let missing = reqwest::Client::new()
        .get(format!("{base}/api/v1/manager/digest?scope=project:ghost"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
}

/// Why: at least one test must drive real HTTP-client mechanics through
/// `MockInferenceServer` (loopback, no live key) — the digest end-to-end over the
/// actual `OpenAiCompatAdapter`.
/// Test: itself.
#[tokio::test]
async fn manager_digest_over_mock_inference_server() {
    let server = MockInferenceServer::spawn(200, mock_body("Mocked portfolio digest."))
        .await
        .expect("spawn mock");
    let state = fresh_state().await;
    register_project(&state, "alpha", "https://github.com/acme/alpha").await;
    state
        .manager_state()
        .inference()
        .set_adapter(mock_adapter(&server), "openai/gpt-4o-mini");
    let base = serve(Arc::clone(&state)).await;

    let resp = reqwest::Client::new()
        .get(format!("{base}/api/v1/manager/digest"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["generated_by"], "llm");
    assert_eq!(body["narrative"], "Mocked portfolio digest.");

    // The adapter really POSTed to the mock's /chat/completions.
    let captured = server.last_request().expect("mock received a request");
    assert_eq!(captured.method, "POST");
    assert_eq!(captured.path, "/chat/completions");
}

// ── Chat ───────────────────────────────────────────────────────────────────────

/// Why: multi-turn chat must thread conversation state per key — distinct scripted
/// replies flow back and the retained turn count grows across turns.
/// Test: itself.
#[tokio::test]
async fn chat_multi_turn_conversation_continuity() {
    let state = fresh_state().await;
    register_project(&state, "alpha", "https://github.com/acme/alpha").await;
    install_scripted(
        &state,
        ScriptedAdapter::new("scripted", capabilities(ProviderId::OpenRouter))
            .with_response(scripted_reply("First reply."))
            .with_response(scripted_reply("Second reply, with context.")),
    );
    let base = serve(Arc::clone(&state)).await;
    let client = reqwest::Client::new();

    let first: Value = client
        .post(format!("{base}/api/v1/manager/chat"))
        .json(&json!({"conversation_key": "conv-1", "message": "what's up?"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(first["conversation_key"], "conv-1");
    assert_eq!(first["reply"], "First reply.");
    assert_eq!(first["turn_count"], 2);

    let second: Value = client
        .post(format!("{base}/api/v1/manager/chat"))
        .json(&json!({"conversation_key": "conv-1", "message": "and now?"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(second["reply"], "Second reply, with context.");
    assert_eq!(second["turn_count"], 4, "turn count grows within the key");
}

/// Why: chat with no provider configured returns a typed 503 (no deterministic
/// reply substitute exists for a conversation).
/// Test: itself.
#[tokio::test]
async fn chat_degrades_without_provider() {
    let state = fresh_state().await;
    state.manager_state().inference().set_unconfigured();
    let base = serve(Arc::clone(&state)).await;

    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/manager/chat"))
        .json(&json!({"conversation_key": "conv-x", "message": "hello"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"], "inference_unavailable");
}

/// Why: even though phase 2 (#2586) lets chat PROPOSE an action, the request
/// must still carry NO `tools` (the model's only action surface is the parsed
/// `manager-action` text sentinel, never real tool-calling), and a PLAIN reply
/// with no confirmed proposal must not create or change any session/Deliverable
/// record — the mock here replies with ordinary prose (no proposal block), so
/// this pins the still-true "a plain message never mutates" half of the
/// boundary. The propose→confirm execution path itself is covered by the
/// dedicated suite in `tests/manager_routing.rs`.
/// Test: itself.
#[tokio::test]
async fn chat_plain_reply_carries_no_tools_and_causes_no_mutation() {
    let server = MockInferenceServer::spawn(200, mock_body("Read-only answer."))
        .await
        .expect("spawn mock");
    let state = fresh_state().await;
    register_project(&state, "alpha", "https://github.com/acme/alpha").await;
    seed_session(&state, "https://github.com/acme/alpha", "a-1").await;
    seed_deliverable(&state, "alpha", DeliverableStatus::InProgress).await;
    state
        .manager_state()
        .inference()
        .set_adapter(mock_adapter(&server), "openai/gpt-4o-mini");
    let base = serve(Arc::clone(&state)).await;
    let client = reqwest::Client::new();

    // Snapshot the deterministic totals BEFORE the chat turn.
    let before: Value = client
        .get(format!("{base}/api/v1/manager/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let reply = client
        .post(format!("{base}/api/v1/manager/chat"))
        .json(&json!({"conversation_key": "conv-ro", "message": "launch a session for me"}))
        .send()
        .await
        .unwrap();
    assert_eq!(reply.status(), reqwest::StatusCode::OK);

    // The request the manager sent must carry NO tools — structurally read-only.
    let captured = server.last_request().expect("mock received a request");
    let sent = captured.body.expect("json body");
    assert!(
        sent.get("tools").is_none() || sent["tools"].is_null(),
        "chat must not expose a tool-calling surface: {sent}"
    );

    // Totals are byte-identical after the chat turn — zero mutation of #2108 records.
    let after: Value = client
        .get(format!("{base}/api/v1/manager/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        before["totals"], after["totals"],
        "chat mutated portfolio state"
    );
}

/// Why: over real HTTP, a second turn must carry the FIRST turn's user message and
/// reply back to the provider — proving history is threaded, not dropped.
/// Test: itself.
#[tokio::test]
async fn chat_over_mock_inference_server_threads_history() {
    let server = MockInferenceServer::spawn(200, mock_body("ack"))
        .await
        .expect("spawn mock");
    let state = fresh_state().await;
    register_project(&state, "alpha", "https://github.com/acme/alpha").await;
    state
        .manager_state()
        .inference()
        .set_adapter(mock_adapter(&server), "openai/gpt-4o-mini");
    let base = serve(Arc::clone(&state)).await;
    let client = reqwest::Client::new();

    client
        .post(format!("{base}/api/v1/manager/chat"))
        .json(&json!({"conversation_key": "conv-h", "message": "first question"}))
        .send()
        .await
        .unwrap();
    client
        .post(format!("{base}/api/v1/manager/chat"))
        .json(&json!({"conversation_key": "conv-h", "message": "second question"}))
        .send()
        .await
        .unwrap();

    let requests = server.requests();
    assert_eq!(requests.len(), 2, "one upstream call per chat turn");
    let second = requests[1].body.clone().expect("json body");
    let serialized = second.to_string();
    assert!(
        serialized.contains("first question"),
        "second request must replay the first user turn: {serialized}"
    );
    assert!(
        serialized.contains("ack"),
        "second request must replay the first assistant reply: {serialized}"
    );
}

// ── Live (opt-in) ───────────────────────────────────────────────────────────────

/// Why: a manual smoke against a REAL provider — gated `#[ignore]` so CI never
/// needs a key or network (run with `--include-ignored` and `OPENROUTER_API_KEY`).
/// Test: itself.
#[tokio::test]
#[ignore = "requires a live provider key + network; run with --include-ignored"]
async fn manager_digest_live_provider() {
    let state = fresh_state().await;
    register_project(&state, "alpha", "https://github.com/acme/alpha").await;
    let base = serve(Arc::clone(&state)).await;
    let resp = reqwest::Client::new()
        .get(format!("{base}/api/v1/manager/digest"))
        .send()
        .await
        .unwrap();
    // With a real key the digest is 200/llm; without one it degrades to 503 — both
    // are non-panicking. Assert only that the surface answered.
    assert!(resp.status().is_success() || resp.status().is_server_error());
}
