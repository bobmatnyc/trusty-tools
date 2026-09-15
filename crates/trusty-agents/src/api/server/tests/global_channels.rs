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

/// The router `serve_with_config` builds for a tokenless loopback daemon that
/// minted `credential`.
fn router_with_credential(credential: &str) -> axum::Router {
    crate::api::server::routes::build_router_with_channel_credential(
        AppState::default(),
        None,
        trusty_common::server::SelfOrigins::default(),
        Some(credential.to_string()),
    )
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

    // #7609 critic HIGH-2: the deprecated listener alias writes `instructions`
    // that reach the wake prompt as TRUSTED text, so it takes the same gate.
    let listeners = json!({"revision": "0000", "listeners": []});
    let app = build_router(AppState::default());
    let response = app
        .oneshot(put("/api/agents/fixture/listeners", None, &listeners))
        .await
        .expect("alias put");
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "tokenless PUT /api/agents/{{name}}/listeners"
    );

    // A WRONG credential is refused just as a missing one is, on every write
    // route the gate covers (critic round 3, LOW).
    for (uri, payload) in [
        ("/api/channels", &body),
        ("/api/agents/fixture/channels", &per_assistant),
        ("/api/agents/fixture/listeners", &listeners),
    ] {
        let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
        let response = app
            .oneshot(put(uri, Some("not-the-token"), payload))
            .await
            .expect("wrong credential");
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "a wrong bearer is refused on {uri}"
        );
    }

    // Reads are unchanged by the gate.
    let app = build_router(AppState::default());
    let response = app
        .oneshot(get("/api/channels", None))
        .await
        .expect("global get");
    assert_eq!(response.status(), StatusCode::OK, "reads stay open");
}

/// A tokenless LOOPBACK daemon mints a credential, discloses it on
/// `/api/config`, and accepts the write the served UI then makes.
///
/// Why (#7609 critic HIGH-4): `tagent --api` defaults tokenless and no sidecar
/// spawn passes `--api-token`, so without this the Channels tab the daemon
/// itself serves would 401 on Save. This drives the exact composition
/// `serve_with_config` builds — the minting rule is asserted separately in
/// `channel_auth::tests::the_minting_rule_follows_the_bind`.
#[tokio::test]
async fn a_minted_credential_lets_the_served_ui_save() {
    let _home_guard = crate::test_env::lock_home();
    let (_home, _config) = seed_home();
    let minted =
        crate::api::server::channel_auth::minted_credential(std::net::Ipv4Addr::LOCALHOST.into())
            .expect("a loopback bind mints");

    // The served UI's bootstrap probe hands it over.
    let app = router_with_credential(&minted);
    let config = body_json(app.oneshot(get("/api/config", None)).await.expect("config")).await;
    assert_eq!(config["auth_required"], json!(false), "no operator token");
    assert_eq!(
        config["channel_write_token"].as_str(),
        Some(minted.as_str()),
        "the UI learns the credential it must present"
    );

    // And the write it makes with that credential is admitted.
    let app = router_with_credential(&minted);
    let view = body_json(app.oneshot(get("/api/channels", None)).await.expect("get")).await;
    let revision = view["revision"].as_str().expect("revision").to_owned();
    let mut channels = view["channels"].clone();
    channels
        .as_array_mut()
        .expect("array")
        .push(slack_channel());
    let app = router_with_credential(&minted);
    let response = app
        .oneshot(put(
            "/api/channels",
            Some(&minted),
            &json!({"revision": revision, "channels": channels}),
        ))
        .await
        .expect("put");
    assert_eq!(response.status(), StatusCode::OK, "the served UI can save");

    // A caller without it is still refused.
    let app = router_with_credential(&minted);
    let response = app
        .oneshot(put(
            "/api/channels",
            None,
            &json!({"revision": "0000", "channels": []}),
        ))
        .await
        .expect("no credential");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

/// A daemon with a CONFIGURED token never discloses it.
///
/// Why (#7609 critic round 3, CRITICAL): `/api/config` is exempt from
/// `auth_middleware`, and an earlier revision returned the operator's own API
/// token there whenever no `Origin` header was present — so
/// `curl http://<lan-bind>:port/api/config` handed an unauthenticated,
/// off-host caller the credential for the WHOLE API. Two things fix it, and
/// this pins both: only a MINTED credential is ever disclosed, and a
/// non-loopback bind mints none.
///
/// Pre-fix (9751c0164) this fails at the first assertion: the body carried
/// `channel_write_token` equal to the operator token.
#[tokio::test]
async fn a_configured_token_is_never_disclosed_on_the_config_probe() {
    let _home_guard = crate::test_env::lock_home();
    let (_home, _config) = seed_home();

    // A NON-LOOPBACK, tokened daemon mints nothing, so it discloses nothing —
    // to an unauthenticated caller sending no `Origin`, which is `curl`.
    let lan: std::net::IpAddr = std::net::Ipv4Addr::new(192, 168, 1, 10).into();
    assert_eq!(
        crate::api::server::channel_auth::minted_credential(lan),
        None,
        "a LAN bind mints nothing to disclose"
    );
    let app = crate::api::server::routes::build_router_with_channel_credential(
        AppState::default(),
        Some(TOKEN.into()),
        trusty_common::server::SelfOrigins::default(),
        crate::api::server::channel_auth::minted_credential(lan),
    );
    let config = body_json(app.oneshot(get("/api/config", None)).await.expect("config")).await;
    assert!(
        config.get("channel_write_token").is_none(),
        "no credential is disclosed at all: {config}"
    );
    assert_eq!(config["auth_required"], json!(true));
    assert!(
        !config.to_string().contains(TOKEN),
        "and the operator credential appears nowhere in the body: {config}"
    );

    // A LOOPBACK tokened daemon discloses ONLY the minted credential, never the
    // operator's — the served UI can save without holding the operator's secret.
    let minted =
        crate::api::server::channel_auth::minted_credential(std::net::Ipv4Addr::LOCALHOST.into())
            .expect("a loopback bind mints");
    let app = crate::api::server::routes::build_router_with_channel_credential(
        AppState::default(),
        Some(TOKEN.into()),
        trusty_common::server::SelfOrigins::default(),
        Some(minted.clone()),
    );
    let config = body_json(app.oneshot(get("/api/config", None)).await.expect("config")).await;
    assert_eq!(
        config["channel_write_token"].as_str(),
        Some(minted.as_str())
    );
    assert!(
        !config.to_string().contains(TOKEN),
        "the operator credential still appears nowhere: {config}"
    );

    // And the operator's own credential is still accepted on a write.
    //
    // The MINTED one is not sufficient on a TOKENED daemon, and that is the
    // layering rather than a gap: `auth_middleware` wraps every `/api/*` route
    // from the outside when a token is configured, so a request carrying only
    // the minted credential is refused before `ChannelWriter` runs. The minted
    // credential is what a TOKENLESS daemon's UI uses; on a tokened daemon the
    // UI already holds the operator token for every other call and presents that.
    // `ChannelWriter` accepts either, which is what keeps the gate honest if
    // the middleware layering ever changes.
    for (credential, expected) in [
        (TOKEN, StatusCode::OK),
        (minted.as_str(), StatusCode::UNAUTHORIZED),
    ] {
        let app = crate::api::server::routes::build_router_with_channel_credential(
            AppState::default(),
            Some(TOKEN.into()),
            trusty_common::server::SelfOrigins::default(),
            Some(minted.clone()),
        );
        let view = body_json(
            app.oneshot(get("/api/channels", Some(TOKEN)))
                .await
                .expect("get"),
        )
        .await;
        let revision = view["revision"].as_str().expect("revision").to_owned();
        let app = crate::api::server::routes::build_router_with_channel_credential(
            AppState::default(),
            Some(TOKEN.into()),
            trusty_common::server::SelfOrigins::default(),
            Some(minted.clone()),
        );
        let response = app
            .oneshot(put(
                "/api/channels",
                Some(credential),
                &json!({"revision": revision, "channels": view["channels"].clone()}),
            ))
            .await
            .expect("put");
        assert_eq!(response.status(), expected);
    }

    // `ChannelWriter` itself accepts the minted credential: with no operator
    // token in play there is no middleware in front of it, and it admits the
    // write.
    let app = router_with_credential(&minted);
    let view = body_json(app.oneshot(get("/api/channels", None)).await.expect("get")).await;
    let revision = view["revision"].as_str().expect("revision").to_owned();
    let app = router_with_credential(&minted);
    let response = app
        .oneshot(put(
            "/api/channels",
            Some(&minted),
            &json!({"revision": revision, "channels": view["channels"].clone()}),
        ))
        .await
        .expect("put");
    assert_eq!(response.status(), StatusCode::OK);
}

/// The minted credential is withheld from an origin this daemon would not
/// serve its own UI to.
///
/// Why: `/api/config` is the pre-auth bootstrap probe, so the disclosure is
/// narrowed by the same same-origin test the CORS layer applies — belt and
/// braces with that layer, which already withholds the response BODY from a
/// cross-origin page by refusing to reflect its origin.
#[tokio::test]
async fn the_channel_credential_is_withheld_from_a_foreign_origin() {
    let _home_guard = crate::test_env::lock_home();
    let (_home, _config) = seed_home();
    let minted = "c0ffee".to_string();

    let foreign = Request::builder()
        .method(Method::GET)
        .uri("/api/config")
        .header(header::ORIGIN, "https://evil.example.com")
        .body(Body::empty())
        .expect("request");
    let config = body_json(
        router_with_credential(&minted)
            .oneshot(foreign)
            .await
            .expect("config"),
    )
    .await;
    assert!(
        config.get("channel_write_token").is_none(),
        "a foreign origin learns nothing: {config}"
    );

    let same = Request::builder()
        .method(Method::GET)
        .uri("/api/config")
        .header(header::ORIGIN, "http://127.0.0.1:8080")
        .body(Body::empty())
        .expect("request");
    let config = body_json(
        router_with_credential(&minted)
            .oneshot(same)
            .await
            .expect("config"),
    )
    .await;
    assert_eq!(config["channel_write_token"].as_str(), Some("c0ffee"));
}

/// A broken `<name>.channels.json` does not break the listener alias.
///
/// Why (#7609 critic MEDIUM-5): an earlier revision routed the alias `GET`
/// through the channel view, which also parses the channels file — turning a
/// previously-200 listeners read into a 500 for a file this route never needed.
#[tokio::test]
async fn a_broken_channels_file_does_not_break_the_listener_alias() {
    let _home_guard = crate::test_env::lock_home();
    let (home, _config) = seed_home();
    let agents = home.path().join(".trusty-agents/agents");
    std::fs::create_dir_all(&agents).expect("agents dir");
    std::fs::write(agents.join("fixture.toml"), "[agent]\nname='fixture'\n").expect("manifest");
    // A provider with no adapter: `agent_channels::load_at` refuses this file.
    std::fs::write(
        agents.join("fixture.channels.json"),
        r#"[{"id":"team","name":"Team","provider":"notion","target":"page","enabled":true}]"#,
    )
    .expect("channels");

    let app = build_router(AppState::default());
    let response = app
        .oneshot(get("/api/agents/fixture/listeners", None))
        .await
        .expect("alias get");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the listener view does not depend on the channels file"
    );
    let body = body_json(response).await;
    assert!(
        body.get("listeners").is_some() && body.get("revision").is_some(),
        "the pre-merge body shape: {body}"
    );

    // The channel view still reports the broken file, as it always did.
    let app = build_router(AppState::default());
    let response = app
        .oneshot(get("/api/agents/fixture/channels", None))
        .await
        .expect("channels get");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

/// An accepted channel write emits exactly one audit line naming the route,
/// the scope and the binding counts either side of it.
///
/// Pre-change (`origin/main`) this fails at the status assertion: the route
/// does not exist, so nothing is written and nothing is logged.
#[tokio::test]
async fn a_credentialed_channel_write_is_admitted_and_audited() {
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
    drop(_log_guard);
    assert!(
        captured.contains("audit=\"channel-write\""),
        "one audit line per accepted write:\n{captured}"
    );
    assert!(
        captured.contains("route=\"PUT /api/channels\"")
            && captured.contains("scope=\"global\"")
            && captured.contains("assistant=\"-\"")
            && captured.contains("channels_before=1")
            && captured.contains("channels_after=2")
            && captured.contains("token_configured=true"),
        "the audit line names route, scope, assistant and counts:\n{captured}"
    );
    // #7609 critic MEDIUM-2: `oneshot` carries no `ConnectInfo`, so the caller
    // address is genuinely unknown here and the line says so rather than
    // omitting the field. `serve_with_config` wires
    // `into_make_service_with_connect_info`, which is what fills it in a real
    // daemon.
    assert!(
        captured.contains("remote_addr=\"unknown\""),
        "the address field is present and honest off the wire:\n{captured}"
    );

    // #7609: the revision the write answers with survives the TOML round trip,
    // so a client can continue from it without re-reading.
    let answered = body_json(response).await["revision"]
        .as_str()
        .expect("revision")
        .to_owned();
    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let reread = body_json(
        app.oneshot(get("/api/channels", Some(TOKEN)))
            .await
            .expect("re-get"),
    )
    .await;
    assert_eq!(
        reread["revision"].as_str(),
        Some(answered.as_str()),
        "the revision is stable across the TOML round trip"
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
    let empty = revision(&[]).expect("an empty list encodes");
    let error = persist(&path, &empty, &[]).expect_err("a broken table blocks the write");
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
        persist(&absent, &empty, &[]).expect("absent file"),
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

/// A turn-originated listener patch is refused when this process holds no
/// channel-write credential.
///
/// Why (#7609 critic HIGH-1): `settings.patch/listeners` writes the same
/// `instructions` the gated alias writes, from a model turn, and sat three
/// lines above the arm that was already gated.
#[tokio::test]
async fn a_turn_originated_listener_patch_takes_the_gate() {
    let _home_guard = crate::test_env::lock_home();
    let (_home, _config) = seed_home();
    let restore = crate::api::server::channel_auth::daemon_credential();
    crate::api::server::channel_auth::record_daemon_credential(None);

    let refusal = crate::api::server::agent_listeners::write_from_turn(
        "fixture",
        serde_json::from_value(json!({"revision": "0000", "listeners": []})).expect("update"),
    )
    .await
    .expect_err("a process with no credential refuses");
    assert_eq!(refusal.0, StatusCode::UNAUTHORIZED);

    crate::api::server::channel_auth::record_daemon_credential(restore);
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
