//! `POST /api/channels/{id}/inbound` and the stub channel provider (#8036,
//! #8037).
//!
//! Why: these two seams exist so the channel surface can be verified without a
//! live Slack, Telegram or Gmail credential, and a seam nobody drives is a
//! seam nobody trusts. Each test here is the end-to-end run the issue asked
//! for, against a sandboxed `$HOME` rather than a real account.
//! What: `seed_home` writes a `config.toml` and an assistant roster into a
//! tempdir and points `$HOME` at it; `crate::test_env::lock_home` serializes
//! that against every other `$HOME`-reading test, and
//! `#[serial_test::serial(channel_credentials)]` serializes the stub
//! provider's environment switch against the adapter tests that count
//! registered providers.
//! Test: this module IS the test.

use super::super::agent_channels::inbound::InboundDispatch;
use super::super::channel_inbound::{Injection, inject_with};
use super::super::routes::build_router_with_config;
use super::super::state::AppState;
use crate::channels::credentials::test_env::EnvVarGuard;
use crate::channels::stub;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use serde_json::{Value, json};
use std::sync::Mutex;
use tower::ServiceExt;

const TOKEN: &str = "test-token";

/// A `config.toml` declaring one stub channel that routes to ONE of the two
/// assistants the fixture creates.
///
/// Why the comment: `a_global_write_keeps_a_comment_block_unrelated_to_channels`
/// states the same property for the whole-list write; this fixture carries one
/// so the per-channel writes in `global_channels` can be pointed at a file that
/// has something to lose.
const STUB_CONFIG: &str = "\
# operator comment that must survive a channel write

[[channels]]
id = \"stub-desk\"
name = \"Stub Desk\"
provider = \"stub\"
target = \"desk\"
enabled = true
send_enabled = true
receive_enabled = true
route_to = [\"alpha-assistant\"]
";

/// Point `$HOME` at a tempdir holding `config` and two assistants.
fn seed_home(config: &str) -> tempfile::TempDir {
    seed_home_with(config, &["alpha-assistant", "beta-assistant"])
}

/// [`seed_home`] with the roster named by the caller (#8190).
///
/// Why: the per-assistant Telegram bot rule is stated about two NAMED
/// assistants, and a test that says `alpha`/`beta` cannot be read against the
/// spec. Everything else about the fixture is identical.
fn seed_home_with(config: &str, names: &[&str]) -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("tempdir");
    let dir = home.path().join(".trusty-agents");
    std::fs::create_dir_all(&dir).expect("config dir");
    std::fs::write(dir.join("config.toml"), config).expect("seed config");
    for name in names {
        let agent = dir.join("agents").join(name);
        std::fs::create_dir_all(&agent).expect("assistant dir");
        std::fs::write(
            agent.join("agent.toml"),
            format!("[agent]\nname = \"{name}\"\n"),
        )
        .expect("assistant manifest");
    }
    unsafe {
        std::env::set_var("HOME", home.path());
    }
    home
}

fn injection(text: &str) -> Injection {
    serde_json::from_value(json!({ "text": text })).expect("injection body")
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

fn request(method: Method, uri: &str, token: Option<&str>, body: Option<&Value>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(t) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    builder
        .body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
        .expect("request")
}

/// A dispatcher that records the wake it was handed instead of spawning one.
///
/// Why: the live `SpawnDispatch` calls `run_pm_task_with_persona`, which needs
/// model credentials and would start a real turn from a unit test. Everything
/// above it — the roster read, the global-config read, the source selection —
/// is the live code, which is the whole point of the
/// `receive_inbound_with` seam.
#[derive(Default)]
struct RecordingDispatch {
    woken: Mutex<Vec<(String, String)>>,
}

#[async_trait::async_trait]
impl InboundDispatch for RecordingDispatch {
    async fn dispatch(
        &self,
        agent: &str,
        binding_id: &str,
        _wake: crate::channels::WakePrompt,
        _root: &std::path::Path,
        _user: &crate::rbac::UserIdentity,
    ) {
        self.woken
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((agent.to_string(), binding_id.to_string()));
    }
}

/// An injected event wakes EXACTLY the assistant the global channel routes to.
///
/// Why (#8036): this is the behaviour #7609 shipped and nothing could verify —
/// `receive_inbound`'s three callers all need a live provider credential. The
/// assertion that matters is the pair: `alpha-assistant` woke through
/// `global-route-to`, and `beta-assistant`, which exists on this host and is
/// simply not named, woke not at all.
///
/// Pre-change this test does not compile: there is no `channel_inbound` module
/// to inject through, and no stub provider for the channel to declare.
#[tokio::test]
#[serial_test::serial(channel_credentials)]
async fn an_injected_event_wakes_only_the_routed_assistant() {
    let _home_guard = crate::test_env::lock_home();
    let _home = seed_home(STUB_CONFIG);
    let _stub = EnvVarGuard::set(stub::ENABLE_ENV, "1");

    let dispatcher = RecordingDispatch::default();
    let outcome = inject_with("stub-desk", injection("are you there"), &dispatcher)
        .await
        .expect("the injection is accepted");

    assert_eq!(outcome["claimed"], json!(true));
    assert_eq!(outcome["dispatched"], json!(true));
    assert_eq!(
        outcome["source"],
        json!("global-route-to"),
        "the routed global is the source that fired: {outcome}"
    );
    assert_eq!(
        outcome["woken"],
        json!(["alpha-assistant"]),
        "exactly the routed assistant, and nobody else: {outcome}"
    );
    assert_eq!(outcome["provider"], json!("stub"));

    // The dispatcher saw the same one turn, through the channel's own id.
    assert_eq!(
        dispatcher
            .woken
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone(),
        vec![("alpha-assistant".to_string(), "stub-desk".to_string())]
    );
}

/// #8190 round-2 finding 3: the `allowed_personas` narrowing a per-assistant
/// Telegram bot applies is DISPATCH behaviour, and the only test for it asserted
/// that `TelegramBot::owners()` returns what it was constructed with — which
/// states nothing about who wakes.
///
/// Why this shape: both assistants bind the SAME chat id, which is the case the
/// owner ruling is about — izzie's bot and cto-assistant's bot are different
/// bots, so an update arriving on izzie's must reach izzie alone even though
/// cto-assistant's binding addresses that chat id just as well. The roster, the
/// per-assistant channel files and the global config are all read live through
/// `receive_inbound_with` into `receive_inbound_at`; only the model turn is
/// substituted.
///
/// Pre-change the narrowing is untested, and reverting
/// `receive_inbound_at`'s `allowed` gate fails this: cto-assistant wakes too.
#[tokio::test]
#[serial_test::serial(channel_credentials)]
async fn a_telegram_bots_owners_are_the_only_assistants_it_wakes() {
    let _home_guard = crate::test_env::lock_home();
    let home = seed_home_with("", &["izzie", "cto-assistant"]);
    // The repo this suite runs in ships its own `izzie` under the CWD-relative
    // agents dir, which `agents_dir_candidates` puts FIRST — so the roster has
    // to be pinned to the fixture or `load_at` reads the bundled izzie's
    // (bindingless) channels instead.
    let _agents_dir = EnvVarGuard::set(
        "TAGENT_CONFIG_DIR",
        home.path()
            .join(".trusty-agents/agents")
            .to_str()
            .expect("utf-8 tempdir"),
    );
    for name in ["izzie", "cto-assistant"] {
        std::fs::write(
            home.path()
                .join(".trusty-agents/agents")
                .join(name)
                .join("agent.channels.json"),
            json!([{
                "id": format!("tg-{name}"), "name": "Masa DM", "provider": "telegram",
                "target": "123456", "enabled": true, "send_enabled": true,
                "receive_enabled": true
            }])
            .to_string(),
        )
        .expect("seed bindings");
    }

    let event = crate::listeners::store::StoredEvent {
        id: "telegram:123456:42".into(),
        listener_id: "telegram".into(),
        provider: "telegram".into(),
        event_type: "message.private".into(),
        ts: "2026-09-16T00:00:00Z".into(),
        from: Some("Masa".into()),
        subject: None,
        snippet: Some("Move the 3pm".into()),
        included: true,
        labels: vec![],
    };
    let user = crate::rbac::UserIdentity::from_remote(
        "telegram:123456".to_string(),
        event.from.as_deref(),
        "telegram",
    );
    let dispatcher = RecordingDispatch::default();
    let outcome = super::super::agent_channels::inbound::receive_inbound_with(
        "telegram",
        "123456",
        &event,
        std::path::Path::new("/nonexistent"),
        &user,
        // The bot this update arrived on is bound by izzie alone.
        Some(&["izzie".to_string()]),
        &mut super::super::agent_channels::inbound::DispatchBudget::PerEvent,
        &dispatcher,
    )
    .await;

    assert!(
        outcome.claimed,
        "izzie's own binding claims it: {outcome:?}"
    );
    assert_eq!(
        outcome.woken,
        vec!["izzie".to_string()],
        "exactly the bot's owner, and nobody else: {outcome:?}"
    );
    assert_eq!(
        dispatcher
            .woken
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone(),
        vec![("izzie".to_string(), "tg-izzie".to_string())],
        "one turn, on izzie's own binding — cto-assistant binds the same chat id \
         on ITS bot and must not be woken by this one"
    );
}

/// A channel that is declared but not receiving claims the event and wakes
/// nobody.
///
/// Why (fail-open): `dispatched: false` has to be reachable and distinguishable
/// from an error, or a test that only ever sees success cannot tell a working
/// route from one that dispatches unconditionally.
#[tokio::test]
#[serial_test::serial(channel_credentials)]
async fn an_injection_into_a_disabled_channel_wakes_nobody() {
    let _home_guard = crate::test_env::lock_home();
    let _home =
        seed_home(&STUB_CONFIG.replace("receive_enabled = true", "receive_enabled = false"));
    let _stub = EnvVarGuard::set(stub::ENABLE_ENV, "1");

    let dispatcher = RecordingDispatch::default();
    let outcome = inject_with("stub-desk", injection("are you there"), &dispatcher)
        .await
        .expect("a send-only channel is still a legitimate injection target");
    assert_eq!(outcome["claimed"], json!(true), "{outcome}");
    assert_eq!(outcome["dispatched"], json!(false), "{outcome}");
    assert_eq!(outcome["woken"], json!([]), "{outcome}");

    // An id this host does not declare is a 404, never a silent no-op.
    assert_eq!(
        inject_with("no-such-channel", injection("hello"), &dispatcher)
            .await
            .unwrap_err()
            .0,
        StatusCode::NOT_FOUND
    );
}

/// The injection route takes the channel-write gate, exactly like `PUT`.
///
/// Why (#8036): an injected event spends a model dispatch on an assistant of
/// the caller's choosing. A tokenless daemon must refuse it, and a wrong bearer
/// must be refused as flatly as a missing one.
#[tokio::test]
#[serial_test::serial(channel_credentials)]
async fn an_unauthenticated_injection_is_refused() {
    let _home_guard = crate::test_env::lock_home();
    let _home = seed_home(STUB_CONFIG);
    let _stub = EnvVarGuard::set(stub::ENABLE_ENV, "1");
    let body = json!({"text": "are you there"});

    // No credential configured at all.
    let app = crate::api::server::routes::build_router(AppState::default());
    let response = app
        .oneshot(request(
            Method::POST,
            "/api/channels/stub-desk/inbound",
            None,
            Some(&body),
        ))
        .await
        .expect("tokenless injection");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // A wrong bearer against a daemon that HAS one.
    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let response = app
        .oneshot(request(
            Method::POST,
            "/api/channels/stub-desk/inbound",
            Some("not-the-token"),
            Some(&body),
        ))
        .await
        .expect("wrong credential");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // And a malformed body is refused on its own terms once authorized, so the
    // 401s above are the gate and not a body-parsing accident.
    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let response = app
        .oneshot(request(
            Method::POST,
            "/api/channels/stub-desk/inbound",
            Some(TOKEN),
            Some(&json!({"text": "   "})),
        ))
        .await
        .expect("blank text");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

/// A stub channel sends and reads its own traffic back over HTTP.
///
/// Why (#8037): `POST .../send` answered 502 "Channel credential could not be
/// resolved" and `GET .../messages` answered 502 "Channel history could not be
/// read" on any host without a real provider secret, so neither endpoint had an
/// end-to-end test at all. Both are driven through the router here, so the
/// route registration and the adapter dispatch are covered together.
///
/// Pre-change this fails at the send: `stub` resolves to no adapter, so the
/// binding cannot even be saved.
#[tokio::test]
#[serial_test::serial(channel_credentials)]
async fn a_stub_channel_sends_and_reads_back_over_http() {
    let _home_guard = crate::test_env::lock_home();
    let home = seed_home(STUB_CONFIG);
    let _stub = EnvVarGuard::set(stub::ENABLE_ENV, "1");

    // #8037 review: the stub outbox is process-global and is never emptied, so
    // this test writes a destination no other test names — see
    // `crate::channels::stub::OUTBOX`. Clearing it instead raced the stub's own
    // round-trip test, which sat in a different `serial_test` group.
    let bindings = json!([{
        "id":"stub-desk","name":"Stub Desk","provider":"stub","target":"http-e2e-desk",
        "enabled":true,"send_enabled":true,"receive_enabled":true
    }]);
    std::fs::write(
        home.path()
            .join(".trusty-agents/agents/alpha-assistant/agent.channels.json"),
        bindings.to_string(),
    )
    .expect("seed bindings");

    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let view = body_json(
        app.oneshot(request(
            Method::GET,
            "/api/agents/alpha-assistant/channels",
            Some(TOKEN),
            None,
        ))
        .await
        .expect("channel view"),
    )
    .await;
    let revision = view["revision"].as_str().expect("revision").to_owned();
    assert!(
        view["providers"]
            .as_array()
            .is_some_and(|list| list.iter().any(|p| p["id"] == json!("stub"))),
        "an admitted stub is offered to the channel view: {view}"
    );

    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let response = app
        .oneshot(request(
            Method::POST,
            "/api/agents/alpha-assistant/channels/stub-desk/send",
            Some(TOKEN),
            Some(&json!({"text": "hello desk", "revision": revision})),
        ))
        .await
        .expect("send");
    assert_eq!(response.status(), StatusCode::OK, "the stub accepts a send");
    assert_eq!(body_json(response).await["ok"], json!(true));

    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let response = app
        .oneshot(request(
            Method::GET,
            "/api/agents/alpha-assistant/channels/stub-desk/messages",
            Some(TOKEN),
            None,
        ))
        .await
        .expect("messages");
    assert_eq!(response.status(), StatusCode::OK);
    let history = body_json(response).await;
    assert_eq!(history["available"], json!(true), "{history}");
    assert_eq!(
        history["messages"]
            .as_array()
            .map(|m| m.iter().map(|v| v["text"].clone()).collect::<Vec<_>>()),
        Some(vec![json!("hello desk")]),
        "the read answers with what the send accepted: {history}"
    );
}
