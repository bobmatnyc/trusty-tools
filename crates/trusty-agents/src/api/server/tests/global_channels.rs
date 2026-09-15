//! `GET`/`PUT /api/channels`, the channel-write authorization gate, and the
//! deprecated `/api/agents/{name}/listeners` aliases (#7609 slice 5).
//!
//! Why: these four behaviours are the whole of the slice's HTTP contract, and
//! every one of them fails against `origin/main` — the global routes do not
//! exist there (404), a tokenless daemon accepts channel writes there, and the
//! listener alias answers without saying it is deprecated. Driving them through
//! `build_router*` rather than the handler functions is deliberate: the gate is
//! a router layer, so a handler-level test would not see it.
//! What: each test sandboxes `$HOME` through `crate::test_env::lock_home` so
//! the global config it reads is a fixture and never the developer's own.
//! Test: this module IS the test.

use super::super::routes::{build_router, build_router_with_config};
use super::super::state::AppState;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;

const TOKEN: &str = "test-token";

/// A `config.toml` carrying an operator comment, an unrelated table, and one
/// legacy `[[listeners]]` entry the global view must absorb.
const FIXTURE_CONFIG: &str = "\
# operator comment that must survive a channel write
[mcp]
inject_for_roles = [\"ctrl\"]

[[listeners]]
name = \"gmail-personal\"
connector = \"gmail\"
identity = \"bob-personal\"
enabled = true
";

/// Point `$HOME` at a fresh tempdir holding [`FIXTURE_CONFIG`].
///
/// Why: `GlobalConfig::load` resolves `$HOME/.trusty-agents/config.toml`, so a
/// test that does not sandbox it reads and WRITES the developer's own config.
/// What: returns the tempdir (kept alive by the caller) and the config path.
/// The `$HOME` lock is the caller's, taken before this runs.
fn seed_home() -> (tempfile::TempDir, std::path::PathBuf) {
    let home = tempfile::tempdir().expect("tempdir");
    let dir = home.path().join(".trusty-agents");
    std::fs::create_dir_all(&dir).expect("config dir");
    let path = dir.join("config.toml");
    std::fs::write(&path, FIXTURE_CONFIG).expect("seed config");
    unsafe {
        std::env::set_var("HOME", home.path());
    }
    (home, path)
}

fn get(uri: &str, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method(Method::GET).uri(uri);
    if let Some(t) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    builder.body(Body::empty()).expect("request")
}

fn put(uri: &str, token: Option<&str>, body: &Value) -> Request<Body> {
    let mut builder = Request::builder()
        .method(Method::PUT)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(t) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    builder.body(Body::from(body.to_string())).expect("request")
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

fn slack_channel() -> Value {
    json!({
        "id": "team",
        "name": "Team",
        "provider": "slack",
        "target": "C123456",
        "enabled": true,
        "send_enabled": true
    })
}

/// `GET`/`PUT /api/channels` round-trips the global list under
/// compare-and-swap, and the write preserves the rest of `config.toml`.
///
/// Pre-change (`origin/main`) this fails on the first assertion: neither route
/// is registered, so the `GET` answers 404.
#[tokio::test]
async fn global_channels_round_trip_preserves_config_and_rejects_a_stale_revision() {
    let _home_guard = crate::test_env::lock_home();
    let (_home, config_path) = seed_home();
    let token = Some(TOKEN);

    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let response = app.oneshot(get("/api/channels", token)).await.expect("get");
    assert_eq!(response.status(), StatusCode::OK, "GET /api/channels");
    let view = body_json(response).await;
    assert_eq!(view["scope"], json!("global"));
    assert_eq!(
        view["channels"]
            .as_array()
            .map(|c| c.iter().map(|v| v["id"].clone()).collect::<Vec<_>>()),
        Some(vec![json!("gmail-personal")]),
        "the legacy [[listeners]] entry is absorbed into the global view"
    );
    let revision = view["revision"].as_str().expect("revision").to_owned();

    // A stale revision is a conflict, never a silent overwrite.
    let stale = json!({"revision": "0000", "channels": [slack_channel()]});
    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let response = app
        .oneshot(put("/api/channels", token, &stale))
        .await
        .expect("stale put");
    assert_eq!(response.status(), StatusCode::CONFLICT, "stale revision");

    // The fresh revision writes, keeping the absorbed entry plus a new one.
    let mut channels = view["channels"].clone();
    channels
        .as_array_mut()
        .expect("array")
        .push(slack_channel());
    let update = json!({"revision": revision, "channels": channels});
    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let response = app
        .oneshot(put("/api/channels", token, &update))
        .await
        .expect("put");
    assert_eq!(response.status(), StatusCode::OK, "fresh revision writes");
    let written = body_json(response).await;
    assert_eq!(
        written["channels"].as_array().map(Vec::len),
        Some(2),
        "the write answers with the stored list"
    );

    let raw = std::fs::read_to_string(&config_path).expect("read back");
    assert!(
        raw.contains("# operator comment that must survive a channel write"),
        "unrelated comments survive the write:\n{raw}"
    );
    assert!(raw.contains("[mcp]"), "unrelated tables survive:\n{raw}");
    assert!(raw.contains("[[channels]]"), "channels published:\n{raw}");
}

/// A daemon with no bearer token configured refuses BOTH channel writes, even
/// from loopback.
///
/// Pre-change (`origin/main`) this fails twice: `PUT /api/channels` answers 404
/// (no such route) and `PUT /api/agents/{name}/channels` reaches its handler.
#[tokio::test]
async fn a_tokenless_daemon_refuses_every_channel_write() {
    let _home_guard = crate::test_env::lock_home();
    let (_home, _config) = seed_home();
    let body = json!({"revision": "0000", "channels": [slack_channel()]});

    let app = build_router(AppState::default());
    let response = app
        .oneshot(put("/api/channels", None, &body))
        .await
        .expect("global put");
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "tokenless PUT /api/channels"
    );

    let per_assistant = json!({"revision": "0000", "bindings": [slack_channel()]});
    let app = build_router(AppState::default());
    let response = app
        .oneshot(put("/api/agents/fixture/channels", None, &per_assistant))
        .await
        .expect("assistant put");
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "tokenless PUT /api/agents/{{name}}/channels"
    );

    // Reads are unchanged by the gate.
    let app = build_router(AppState::default());
    let response = app
        .oneshot(get("/api/channels", None))
        .await
        .expect("global get");
    assert_eq!(response.status(), StatusCode::OK, "reads stay open");
}

/// An accepted channel write emits exactly one audit line naming the route,
/// the scope and the binding counts either side of it.
///
/// Pre-change (`origin/main`) this fails at the status assertion: the route
/// does not exist, so nothing is written and nothing is logged.
#[tokio::test]
async fn a_token_bearing_channel_write_is_admitted_and_audited() {
    let _home_guard = crate::test_env::lock_home();
    let (_home, _config) = seed_home();
    let logs = CaptureWriter::default();
    let _log_guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_ansi(false)
            .finish(),
    );

    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let view = body_json(
        app.oneshot(get("/api/channels", Some(TOKEN)))
            .await
            .expect("get"),
    )
    .await;
    let revision = view["revision"].as_str().expect("revision").to_owned();
    let mut channels = view["channels"].clone();
    channels
        .as_array_mut()
        .expect("array")
        .push(slack_channel());

    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let response = app
        .oneshot(put(
            "/api/channels",
            Some(TOKEN),
            &json!({"revision": revision, "channels": channels}),
        ))
        .await
        .expect("put");
    assert_eq!(response.status(), StatusCode::OK);

    let captured = logs.contents();
    assert!(
        captured.contains("audit=\"channel-write\""),
        "one audit line per accepted write:\n{captured}"
    );
    assert!(
        captured.contains("route=\"PUT /api/channels\"")
            && captured.contains("scope=\"global\"")
            && captured.contains("channels_before=1")
            && captured.contains("channels_after=2")
            && captured.contains("token_configured=true"),
        "the audit line names route, scope, counts and caller identity:\n{captured}"
    );
}

/// The deprecated listener alias still answers, and says so in a header.
///
/// Pre-change (`origin/main`) this fails at the header assertion: the alias is
/// the live route and carries no `Deprecation` header.
#[tokio::test]
async fn the_listeners_alias_answers_with_a_deprecation_header() {
    let _home_guard = crate::test_env::lock_home();
    let (_home, _config) = seed_home();

    let app = build_router(AppState::default());
    let response = app
        .oneshot(get("/api/agents/fixture/listeners", None))
        .await
        .expect("alias get");
    assert_eq!(
        response
            .headers()
            .get("deprecation")
            .and_then(|v| v.to_str().ok()),
        Some("true"),
        "the alias marks itself deprecated whatever it answers"
    );
    assert_eq!(
        response.headers().get("link").and_then(|v| v.to_str().ok()),
        Some("</api/agents/{name}/channels>; rel=\"successor-version\""),
        "and names its successor"
    );
}

/// A `config.toml` that will not parse is REPORTED, never read as "no channels
/// declared".
///
/// Why (fail-open check, #7609): reading a broken file as an empty list would
/// let the very next write publish that emptiness over the operator's real
/// configuration. Both the read and the locked write path refuse instead.
#[test]
fn a_malformed_config_is_reported_not_read_as_empty() {
    use super::super::global_channels::{Persisted, channels_in, persist, revision};

    let refusal = channels_in("[mcp\nnot = toml").expect_err("a broken file must not parse");
    assert_eq!(refusal.0, StatusCode::INTERNAL_SERVER_ERROR);

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[[channels]]\nid = 5\n").expect("seed");
    let error = persist(&path, &revision(&[]), &[]).expect_err("a broken table blocks the write");
    assert!(
        error.to_string().contains("nothing was written"),
        "the refusal says nothing was written: {error}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        "[[channels]]\nid = 5\n",
        "the operator's file is untouched"
    );

    // An absent file is a legitimate empty list, not a failure.
    let absent = dir.path().join("missing.toml");
    assert!(matches!(
        persist(&absent, &revision(&[]), &[]).expect("absent file"),
        Persisted::Written {
            before: 0,
            after: 0
        }
    ));
}

/// Global validation covers the provider bridge, the account-wide target, the
/// send-with-no-destination refusal, and `route_to`.
///
/// Why: a migrated Gmail listener declares the CONNECTOR id `gmail` and an
/// empty target, so validating it the way a per-assistant binding is validated
/// would refuse every mailbox this host already polls.
#[test]
fn global_channel_validation_covers_provider_target_and_routes() {
    use super::super::global_channels::validate;
    use crate::channels::Channel;

    let known = vec!["alice".to_string()];
    let migrated = Channel {
        id: "gmail-personal".into(),
        name: "gmail-personal".into(),
        provider: "gmail".into(),
        enabled: true,
        receive_enabled: true,
        credential_ref: Some("gmail/bob-personal".into()),
        ..Channel::default()
    };
    assert!(
        validate(&migrated, &known).is_ok(),
        "a migrated listener is a valid global channel"
    );

    // Account-wide means it can receive, never that it can send.
    let mut sending = migrated.clone();
    sending.send_enabled = true;
    assert_eq!(
        validate(&sending, &known).expect_err("no destination").0,
        StatusCode::BAD_REQUEST
    );

    // A named destination is still held to the provider's grammar.
    let mut misaddressed = migrated.clone();
    misaddressed.target = "alice@example.com".into();
    assert_eq!(
        validate(&misaddressed, &known).expect_err("bad target").0,
        StatusCode::BAD_REQUEST
    );

    // `route_to` may only name assistants this host has.
    let mut misrouted = migrated.clone();
    misrouted.route_to = vec!["nobody".into()];
    assert_eq!(
        validate(&misrouted, &known).expect_err("unknown route").0,
        StatusCode::BAD_REQUEST
    );
    misrouted.route_to = vec!["alice".into()];
    assert!(validate(&misrouted, &known).is_ok());

    // An unregistered provider is refused, as on the per-assistant route.
    let mut unsupported = migrated;
    unsupported.provider = "notion".into();
    assert_eq!(
        validate(&unsupported, &known).expect_err("no adapter").0,
        StatusCode::BAD_REQUEST
    );
}

/// A `tracing` writer that keeps every emitted line in memory.
///
/// Why: the audit line is the only observable of an accepted write, so the
/// test needs the formatted output rather than a mock.
#[derive(Clone, Default)]
struct CaptureWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl CaptureWriter {
    fn contents(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap_or_else(|e| e.into_inner())).into_owned()
    }
}

impl std::io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}
