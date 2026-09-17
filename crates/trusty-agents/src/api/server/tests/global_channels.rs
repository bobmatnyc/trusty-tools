//! `GET`/`PUT /api/channels` and the channel-write authorization gate (#7609
//! slices 5 and 7).
//!
//! Why: these behaviours are the whole of the global surface's HTTP contract.
//! Driving them through `build_router*` rather than the handler functions is
//! deliberate: the gate is a router layer, so a handler-level test would not
//! see it, and the retired listener routes can only be shown absent from the
//! router the daemon actually serves.
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
/// global channel in its migrated form.
///
/// #7609 slice 7: this used to declare `[[listeners]]` and rely on the parse
/// absorbing it. The absorb is retired — the startup drain moves the table on
/// disk and deletes it — so the fixture declares what a drained file declares.
const FIXTURE_CONFIG: &str = "\
# operator comment that must survive a channel write
[mcp]
inject_for_roles = [\"ctrl\"]

[[channels]]
id = \"gmail-personal\"
name = \"gmail-personal\"
provider = \"gmail\"
target = \"\"
enabled = true
send_enabled = false
receive_enabled = true
credential_ref = \"gmail/bob-personal\"
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
    with_body(Method::PUT, uri, token, body)
}

/// #8038: the per-channel create takes the same shape as [`put`].
fn post(uri: &str, token: Option<&str>, body: &Value) -> Request<Body> {
    with_body(Method::POST, uri, token, body)
}

/// #8187: the per-channel delete carries its revision and `force` flag in the
/// query string, because a `DELETE` body is not something a client can rely on.
fn delete_req(uri: &str, token: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().method(Method::DELETE).uri(uri);
    if let Some(t) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    builder.body(Body::empty()).expect("request")
}

fn with_body(method: Method, uri: &str, token: Option<&str>, body: &Value) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
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
        "the declared global channel is what the view lists"
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

    // A WRONG credential is refused just as a missing one is, on every write
    // route the gate covers (critic round 3, LOW).
    for (uri, payload) in [
        ("/api/channels", &body),
        ("/api/agents/fixture/channels", &per_assistant),
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
            && captured.contains("channels_before=\"1\"")
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

/// The retired listener routes are unregistered, and the router answers for
/// them instead of a handler.
///
/// Why (#7609 slice 7): the aliases were kept for one release and no published
/// release of this crate ever carried them (`git tag --list 'trusty-agents-v*'`
/// is empty), so they are deleted. What matters is HOW they are gone: the
/// request must complete against the router the daemon actually serves, with no
/// panic and with nothing that looks like the old contract — no listener view,
/// no `Deprecation` header, no write accepted.
///
/// On the `GET` the answer is the SPA catch-all `/{*path}`, which is what every
/// unregistered GET path has always returned; the `PUT` matches no method on
/// that path at all. Neither reaches a handler.
///
/// Pre-change this fails on the first assertion: the alias is registered and
/// answers the listener view with a `Deprecation` header.
#[tokio::test]
async fn the_retired_listener_routes_are_unregistered() {
    let _home_guard = crate::test_env::lock_home();
    let (_home, _config) = seed_home();
    let body = json!({"revision": "0000", "listeners": []});

    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let response = app
        .oneshot(get("/api/agents/fixture/listeners", Some(TOKEN)))
        .await
        .expect("the GET completes without panicking");
    assert!(
        response.headers().get("deprecation").is_none(),
        "no deprecated alias answered it"
    );
    let answered = body_json(response).await;
    assert!(
        answered.get("listeners").is_none() && answered.get("revision").is_none(),
        "the listener view is gone, not merely renamed: {answered}"
    );

    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let response = app
        .oneshot(put("/api/agents/fixture/listeners", Some(TOKEN), &body))
        .await
        .expect("the PUT completes without panicking");
    assert_eq!(
        response.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "no method is registered on the retired path"
    );

    // The successor route IS registered for the same method on the same shape,
    // so the 405 above is the retired path being absent rather than `PUT` being
    // unroutable under `/api/agents/{name}/`.
    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let response = app
        .oneshot(put(
            "/api/agents/fixture/channels",
            Some(TOKEN),
            &json!({"revision": "0000", "bindings": []}),
        ))
        .await
        .expect("channels put");
    assert_ne!(
        response.status(),
        StatusCode::METHOD_NOT_ALLOWED,
        "the successor route accepts the method the retired one no longer does"
    );
}

/// `POST /api/channels` declares ONE channel and `PUT /api/channels/{id}`
/// replaces ONE, both under the revision the client read.
///
/// Why (#8038): the whole-list `PUT` made a scripted create mean "send back
/// every channel you did not intend to touch" — omit one and it is deleted.
/// These two routes do the read-modify-write server-side, so the refusals worth
/// pinning are the ones the list shape never had: a duplicate ID, an unknown
/// ID, and a body that renames the channel in the path.
///
/// Pre-change this fails on the first assertion: `POST /api/channels` is not
/// registered, so the router answers 405.
#[tokio::test]
async fn a_global_channel_is_created_and_updated_one_at_a_time() {
    let _home_guard = crate::test_env::lock_home();
    let (_home, config_path) = seed_home();

    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let view = body_json(
        app.oneshot(get("/api/channels", Some(TOKEN)))
            .await
            .expect("get"),
    )
    .await;
    let revision = view["revision"].as_str().expect("revision").to_owned();

    // A stale revision is refused before anything is written.
    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let response = app
        .oneshot(post(
            "/api/channels",
            Some(TOKEN),
            &json!({"revision": "0000", "channel": slack_channel()}),
        ))
        .await
        .expect("stale create");
    assert_eq!(response.status(), StatusCode::CONFLICT, "stale revision");

    // The create keeps the channel that was already declared.
    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let response = app
        .oneshot(post(
            "/api/channels",
            Some(TOKEN),
            &json!({"revision": revision, "channel": slack_channel()}),
        ))
        .await
        .expect("create");
    assert_eq!(response.status(), StatusCode::OK, "the create is accepted");
    let created = body_json(response).await;
    assert_eq!(
        created["channels"]
            .as_array()
            .map(|c| c.iter().map(|v| v["id"].clone()).collect::<Vec<_>>()),
        Some(vec![json!("gmail-personal"), json!("team")]),
        "the new channel is appended, the declared one survives: {created}"
    );
    let revision = created["revision"].as_str().expect("revision").to_owned();

    // A second create of the same ID is a conflict, not a silent replacement.
    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let response = app
        .oneshot(post(
            "/api/channels",
            Some(TOKEN),
            &json!({"revision": revision, "channel": slack_channel()}),
        ))
        .await
        .expect("duplicate create");
    assert_eq!(response.status(), StatusCode::CONFLICT, "duplicate ID");

    // The update replaces that one channel and leaves the other alone.
    let mut renamed = slack_channel();
    renamed["name"] = json!("Team (renamed)");
    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let response = app
        .oneshot(put(
            "/api/channels/team",
            Some(TOKEN),
            &json!({"revision": revision, "channel": renamed}),
        ))
        .await
        .expect("update");
    assert_eq!(response.status(), StatusCode::OK, "the update is accepted");
    let updated = body_json(response).await;
    assert_eq!(
        updated["channels"]
            .as_array()
            .and_then(|c| c.iter().find(|v| v["id"] == json!("team")))
            .map(|v| v["name"].clone()),
        Some(json!("Team (renamed)")),
        "the named channel changed: {updated}"
    );
    assert_eq!(
        updated["channels"].as_array().map(Vec::len),
        Some(2),
        "and nothing else was dropped: {updated}"
    );
    let revision = updated["revision"].as_str().expect("revision").to_owned();

    // An unknown ID is a 404; a body that renames the channel is a 400.
    let mut absent = slack_channel();
    absent["id"] = json!("no-such-channel");
    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let response = app
        .oneshot(put(
            "/api/channels/no-such-channel",
            Some(TOKEN),
            &json!({"revision": revision, "channel": absent}),
        ))
        .await
        .expect("unknown id");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let mut mismatched = slack_channel();
    mismatched["id"] = json!("somewhere-else");
    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let response = app
        .oneshot(put(
            "/api/channels/team",
            Some(TOKEN),
            &json!({"revision": revision, "channel": mismatched}),
        ))
        .await
        .expect("renamed body");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // Everything unrelated to channels is still in the file.
    let raw = std::fs::read_to_string(&config_path).expect("read back");
    assert!(
        raw.contains("# operator comment that must survive a channel write"),
        "unrelated comments survive a per-channel write:\n{raw}"
    );
    assert!(raw.contains("[mcp]"), "unrelated tables survive:\n{raw}");
}

/// The per-channel writes take the SAME gate the whole-list `PUT` takes.
///
/// Why (#8038): a create is a channel write — it decides which destination an
/// assistant reaches and which one reaches it — so a route that accepted one
/// without a credential would be a hole beside a guarded door.
///
/// Pre-change this fails: neither route exists, so both answer 405 rather than
/// 401.
#[tokio::test]
async fn an_unauthenticated_create_is_refused() {
    let _home_guard = crate::test_env::lock_home();
    let (_home, _config) = seed_home();
    let body = json!({"revision": "0000", "channel": slack_channel()});

    for (method, uri) in [
        (Method::POST, "/api/channels"),
        (Method::PUT, "/api/channels/team"),
    ] {
        // A daemon with no credential at all refuses.
        let app = build_router(AppState::default());
        let request = Request::builder()
            .method(method.clone())
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("request");
        let response = app.oneshot(request).await.expect("tokenless write");
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "tokenless {method} {uri}"
        );

        // And a wrong bearer is refused exactly as a missing one is.
        let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
        let request = Request::builder()
            .method(method.clone())
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, "Bearer not-the-token")
            .body(Body::from(body.to_string()))
            .expect("request");
        let response = app.oneshot(request).await.expect("wrong credential");
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "wrong bearer on {method} {uri}"
        );
    }
}

/// #8187: a global channel a per-assistant binding overlays is NOT deleted
/// until the operator asks twice, and the forced delete says what it orphaned.
///
/// Why this is the interesting case: an overlay is a binding with a blank
/// target whose `id` names the global (`channels::dispatch::is_overlay`).
/// Delete the global and `agent_channels::load_at_with` drops that record with
/// a log line nobody reads, so the assistant loses a destination silently.
///
/// Pre-change this fails at the first DELETE: no delete route is registered on
/// `/api/channels/{id}`, so the request answers 405 rather than 409.
#[tokio::test]
async fn a_referenced_global_channel_is_not_deleted_without_force() {
    let _home_guard = crate::test_env::lock_home();
    let (home, config_path) = seed_home();
    let (bindings_path, overlay) =
        seed_overlay_assistant(&home, "overlay-assistant", "gmail-personal");

    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let view = body_json(
        app.oneshot(get("/api/channels", Some(TOKEN)))
            .await
            .expect("get"),
    )
    .await;
    let revision = view["revision"].as_str().expect("revision").to_owned();

    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let response = app
        .oneshot(delete_req(
            &format!("/api/channels/gmail-personal?revision={revision}"),
            Some(TOKEN),
        ))
        .await
        .expect("unforced delete");
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "a bound channel is refused"
    );
    let refusal = body_json(response).await;
    assert_eq!(
        refusal["referenced_by"],
        json!(["overlay-assistant"]),
        "the refusal names who still binds it: {refusal}"
    );
    assert!(
        std::fs::read_to_string(&config_path)
            .expect("read back")
            .contains("gmail-personal"),
        "the refused delete wrote nothing"
    );

    // Forced: the channel goes, the binding stays on disk and is reported.
    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let response = app
        .oneshot(delete_req(
            &format!("/api/channels/gmail-personal?revision={revision}&force=true"),
            Some(TOKEN),
        ))
        .await
        .expect("forced delete");
    assert_eq!(response.status(), StatusCode::OK, "force deletes");
    let deleted = body_json(response).await;
    assert_eq!(
        deleted["inert_bindings"],
        json!(["overlay-assistant"]),
        "the forced delete reports what it left inert: {deleted}"
    );
    assert_eq!(deleted["channels"].as_array().map(Vec::len), Some(0));
    let raw = std::fs::read_to_string(&config_path).expect("read back");
    assert!(
        !raw.contains("gmail-personal"),
        "the channel is gone:\n{raw}"
    );
    assert_eq!(
        std::fs::read_to_string(&bindings_path).expect("bindings"),
        overlay,
        "this route never edits another assistant's file"
    );
}

/// #8187: the unbound delete path — unknown id 404, stale revision 409, and an
/// accepted delete that leaves the rest of `config.toml` alone.
///
/// Pre-change this fails at the first assertion: `/api/channels/{id}` answers
/// PUT only, so a DELETE is a 405.
#[tokio::test]
async fn a_global_channel_is_deleted_and_an_unknown_id_is_404() {
    let _home_guard = crate::test_env::lock_home();
    let (_home, config_path) = seed_home();

    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let view = body_json(
        app.oneshot(get("/api/channels", Some(TOKEN)))
            .await
            .expect("get"),
    )
    .await;
    let revision = view["revision"].as_str().expect("revision").to_owned();

    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let response = app
        .oneshot(delete_req(
            &format!("/api/channels/no-such-channel?revision={revision}"),
            Some(TOKEN),
        ))
        .await
        .expect("unknown id");
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let response = app
        .oneshot(delete_req(
            "/api/channels/gmail-personal?revision=0000",
            Some(TOKEN),
        ))
        .await
        .expect("stale revision");
    assert_eq!(response.status(), StatusCode::CONFLICT, "stale revision");

    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let response = app
        .oneshot(delete_req(
            &format!("/api/channels/gmail-personal?revision={revision}"),
            Some(TOKEN),
        ))
        .await
        .expect("delete");
    assert_eq!(response.status(), StatusCode::OK);
    let deleted = body_json(response).await;
    assert_eq!(deleted["deleted"], json!("gmail-personal"));
    assert_eq!(
        deleted["inert_bindings"],
        json!([]),
        "nothing bound it: {deleted}"
    );
    assert_eq!(deleted["channels"].as_array().map(Vec::len), Some(0));

    let raw = std::fs::read_to_string(&config_path).expect("read back");
    assert!(
        raw.contains("# operator comment that must survive a channel write"),
        "unrelated comments survive a delete:\n{raw}"
    );
    assert!(raw.contains("[mcp]"), "unrelated tables survive:\n{raw}");
}

/// #8187: the delete takes the SAME [`ChannelWriter`] gate the create and the
/// update take — removing a destination is at least as consequential as adding
/// one.
///
/// Pre-change this fails: the route is unregistered, so both cases answer 405
/// rather than 401.
///
/// [`ChannelWriter`]: super::super::channel_auth::ChannelWriter
#[tokio::test]
async fn an_unauthenticated_delete_is_refused() {
    let _home_guard = crate::test_env::lock_home();
    let (_home, _config) = seed_home();
    let uri = "/api/channels/gmail-personal?revision=0000";

    let app = build_router(AppState::default());
    let response = app
        .oneshot(delete_req(uri, None))
        .await
        .expect("tokenless delete");
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "a tokenless daemon refuses the delete"
    );

    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let response = app
        .oneshot(delete_req(uri, Some("not-the-token")))
        .await
        .expect("wrong credential");
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "a wrong bearer is refused exactly as a missing one is"
    );
}

/// #8187 (critic round, HIGH): the delete response says whether a receiver for
/// the deleted channel is still running.
///
/// `listeners::poll::spawn_listeners` runs once at API bootstrap and each poll
/// loop owns a copy of its config, so deleting a `receive_enabled` channel
/// stops nothing — it keeps polling and keeps waking its captured `route_to`
/// until the daemon restarts. A send-only channel has no such loop.
///
/// Pre-change this fails at the first assertion: the response carries no
/// `receiving_until_restart` key, so the comparison is against `Null`.
#[tokio::test]
async fn a_deleted_receiving_channel_says_its_receiver_runs_until_restart() {
    let _home_guard = crate::test_env::lock_home();
    let (_home, _config) = seed_home();

    // A send-only channel beside the fixture's receiving one.
    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let view = body_json(
        app.oneshot(get("/api/channels", Some(TOKEN)))
            .await
            .expect("get"),
    )
    .await;
    let revision = view["revision"].as_str().expect("revision").to_owned();
    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let created = body_json(
        app.oneshot(post(
            "/api/channels",
            Some(TOKEN),
            &json!({"revision": revision, "channel": slack_channel()}),
        ))
        .await
        .expect("post"),
    )
    .await;
    let revision = created["revision"].as_str().expect("revision").to_owned();

    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let deleted = body_json(
        app.oneshot(delete_req(
            &format!("/api/channels/gmail-personal?revision={revision}"),
            Some(TOKEN),
        ))
        .await
        .expect("delete receiving"),
    )
    .await;
    assert_eq!(
        deleted["receiving_until_restart"],
        json!(true),
        "the receiver keeps running and the response says so: {deleted}"
    );

    let revision = deleted["revision"].as_str().expect("revision").to_owned();
    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let deleted = body_json(
        app.oneshot(delete_req(
            &format!("/api/channels/team?revision={revision}"),
            Some(TOKEN),
        ))
        .await
        .expect("delete send-only"),
    )
    .await;
    assert_eq!(
        deleted["receiving_until_restart"],
        json!(false),
        "a send-only channel leaves nothing running: {deleted}"
    );
}

/// #8187 (critic round, MEDIUM): a forced delete records the override itself.
///
/// The shared `ChannelWriter::audit` line carries only the channel counts, so
/// a delete that overrode live bindings and one that overrode nothing were
/// indistinguishable in the log — `force` is the one thing about this request
/// an operator later has to account for.
///
/// Pre-change this fails on the `forced=true` assertion: nothing but the
/// count-only line is emitted.
#[tokio::test]
async fn a_forced_delete_audits_the_override_and_the_orphans() {
    let _home_guard = crate::test_env::lock_home();
    let (home, _config) = seed_home();
    seed_overlay_assistant(&home, "overlay-assistant", "gmail-personal");

    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let view = body_json(
        app.oneshot(get("/api/channels", Some(TOKEN)))
            .await
            .expect("get"),
    )
    .await;
    let revision = view["revision"].as_str().expect("revision").to_owned();

    let logs = CaptureWriter::default();
    let _log_guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_ansi(false)
            .finish(),
    );
    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let response = app
        .oneshot(delete_req(
            &format!("/api/channels/gmail-personal?revision={revision}&force=true"),
            Some(TOKEN),
        ))
        .await
        .expect("forced delete");
    assert_eq!(response.status(), StatusCode::OK);
    let captured = logs.contents();
    drop(_log_guard);

    assert!(
        captured.contains("audit=\"channel-write-forced\"")
            && captured.contains("forced=true")
            && captured.contains("channel=\"gmail-personal\"")
            && captured.contains("inert_bindings=\"overlay-assistant\""),
        "the forced override is its own greppable record:\n{captured}"
    );
}

/// #8187 (critic round, MEDIUM): one unusable assistant directory must not
/// make EVERY global delete a 500 forever, and a delete this route genuinely
/// cannot decide names the assistant that blocked it.
///
/// A roster name whose charset the channels API cannot address is skipped —
/// no operator action reachable from this API clears it. A malformed channels
/// file stays fail-closed, because repairing that file does.
///
/// Pre-change this fails at the first assertion: both cases collapse into the
/// same 500 with no `assistant` field at all, so even the skippable one blocks
/// the delete.
#[tokio::test]
async fn an_unaddressable_assistant_name_is_skipped_and_a_broken_file_is_named() {
    let _home_guard = crate::test_env::lock_home();
    let (home, config_path) = seed_home();
    let agents = home.path().join(".trusty-agents/agents");
    // A directory `candidate_agent_names` enumerates and `config_path` refuses.
    let unaddressable = agents.join("bad name");
    std::fs::create_dir_all(&unaddressable).expect("unaddressable dir");
    std::fs::write(unaddressable.join("agent.toml"), "[agent]\nname = \"x\"\n")
        .expect("unaddressable manifest");
    // A well-named assistant whose channels file will not parse.
    let broken = agents.join("broken");
    std::fs::create_dir_all(&broken).expect("broken dir");
    std::fs::write(broken.join("agent.toml"), "[agent]\nname = \"broken\"\n")
        .expect("broken manifest");
    let broken_bindings = broken.join("agent.channels.json");
    std::fs::write(&broken_bindings, "{not json").expect("broken bindings");

    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let view = body_json(
        app.oneshot(get("/api/channels", Some(TOKEN)))
            .await
            .expect("get"),
    )
    .await;
    let revision = view["revision"].as_str().expect("revision").to_owned();

    let logs = CaptureWriter::default();
    let _log_guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_ansi(false)
            .finish(),
    );
    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let response = app
        .oneshot(delete_req(
            &format!("/api/channels/gmail-personal?revision={revision}&force=true"),
            Some(TOKEN),
        ))
        .await
        .expect("delete over a broken file");
    assert_eq!(
        response.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "a file that will not parse stays fail-closed"
    );
    let refusal = body_json(response).await;
    let captured = logs.contents();
    drop(_log_guard);
    assert_eq!(
        refusal["assistant"],
        json!("broken"),
        "the 500 names the assistant blocking the delete: {refusal}"
    );
    assert!(
        refusal["error"]
            .as_str()
            .is_some_and(|e| e.contains("broken")),
        "and so does its message: {refusal}"
    );
    assert!(
        captured.contains("assistant=\"broken\""),
        "the log names it too:\n{captured}"
    );
    assert!(
        captured.contains("assistant=\"bad name\""),
        "and warns that the unaddressable name was skipped:\n{captured}"
    );
    assert!(
        std::fs::read_to_string(&config_path)
            .expect("read back")
            .contains("gmail-personal"),
        "the refused delete wrote nothing"
    );

    // With the one broken file gone, the unaddressable directory alone does not
    // block the delete — the whole point of skipping it.
    std::fs::remove_file(&broken_bindings).expect("drop the broken file");
    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let response = app
        .oneshot(delete_req(
            &format!("/api/channels/gmail-personal?revision={revision}"),
            Some(TOKEN),
        ))
        .await
        .expect("delete past the unaddressable name");
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "an unaddressable roster name is skipped, not a permanent 500"
    );
}

/// #8187 (critic round, HIGH): the assistant view names the overlays it had to
/// drop, so an orphaned binding is visible somewhere other than the one delete
/// response that created it.
///
/// Lives beside the delete tests because the orphan this reports is the delete's
/// blast radius, and it needs the same sandboxed `$HOME` — `read` resolves the
/// agents dirs from the environment, not from an argument.
///
/// Pre-change this fails at the first assertion: the payload carries no
/// `inert_overlays` key, so the comparison is against `Null`.
#[tokio::test]
async fn the_assistant_channel_view_names_its_inert_overlays() {
    let _home_guard = crate::test_env::lock_home();
    let (home, _config) = seed_home();
    seed_overlay_assistant(&home, "orphan-assistant", "no-such-global");

    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let view = body_json(
        app.oneshot(get("/api/agents/orphan-assistant/channels", Some(TOKEN)))
            .await
            .expect("get"),
    )
    .await;
    assert_eq!(
        view["inert_overlays"],
        json!(["no-such-global"]),
        "the dropped record is named: {view}"
    );
    assert_eq!(
        view["bindings"].as_array().map(Vec::len),
        Some(0),
        "and is still absent from the loadable list: {view}"
    );
}

/// Write `assistant` under `home` with one overlay binding naming `channel`.
///
/// Why: three #8187 tests need the same orphan-able overlay on disk, and a
/// second copy of the fixture is where they start to disagree about what an
/// overlay is (a blank target, keyed by id).
/// What: returns the bindings path and the exact bytes written, so a caller can
/// assert the delete route left another assistant's file alone.
fn seed_overlay_assistant(
    home: &tempfile::TempDir,
    assistant: &str,
    channel: &str,
) -> (std::path::PathBuf, String) {
    let dir = home.path().join(".trusty-agents/agents").join(assistant);
    std::fs::create_dir_all(&dir).expect("assistant dir");
    std::fs::write(
        dir.join("agent.toml"),
        format!("[agent]\nname = \"{assistant}\"\n"),
    )
    .expect("assistant manifest");
    let overlay = json!([{
        "id": channel,
        "name": "Personal mail",
        "provider": "gmail",
        "target": "",
        "enabled": true,
        "receive_enabled": true
    }])
    .to_string();
    let path = dir.join("agent.channels.json");
    std::fs::write(&path, &overlay).expect("assistant bindings");
    (path, overlay)
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

/// A `before` count the audit could not read says so, rather than claiming nil.
#[test]
fn an_unreadable_before_count_audits_as_unknown() {
    let logs = CaptureWriter::default();
    let guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_ansi(false)
            .finish(),
    );
    crate::api::server::channel_auth::audit_write(
        "turn:channels",
        "assistant",
        Some("fixture"),
        None,
        2,
        "in-process",
    );
    let captured = logs.contents();
    drop(guard);
    assert!(
        captured.contains("channels_before=\"unknown\"") && captured.contains("channels_after=2"),
        "an unreadable count is distinguishable from zero:\n{captured}"
    );
}

/// A `config.toml` shaped like the operator's own: a comment block ABOVE the
/// legacy `[[listeners]]` table, a second one between that table and an
/// unrelated `[mcp.*]` table, and a heading above `[[channels]]`.
///
/// Why this exact shape: `toml_edit` files a comment block as the LEADING decor
/// of whatever header follows it, so where the block sits decides which table
/// takes it away when removed. The block above `[[listeners]]` is the one the
/// live `PUT` deleted; the block below it belongs to `[mcp.services]` and must
/// be equally untouched.
const COMMENT_FIXTURE: &str = "\
# trusty-agents global configuration

[mcp]
inject_for_roles = [\"ctrl\"]

# tickets-mcp is intentionally NOT a `driver = \"direct\"` (OpenRPC) endpoint:
# the `tickets-mcp` binary speaks MCP framing, not OpenRPC `rpc.discover`, so a
# direct endpoint would silently fail discovery. It is wired out-of-process via
# the repo-root `.mcp.json` stdio server instead. The former dead `tickets-mcp`
# (and `commons-ticketing`) OpenRPC stubs were retired per ADR-0014 (native
# Rust MCP, PR #2624).

[[listeners]]
name = \"gmail-personal\"
connector = \"gmail\"
identity = \"redacted-account\"
enabled = true

# the stdio services this host spawns
[[mcp.services]]
name = \"trusty-mpm\"
command = \"trusty-mpm\"

# the harness-wide channels
[[channels]]
id = \"gmail-personal\"
name = \"gmail-personal\"
provider = \"gmail\"
target = \"\"
enabled = true
send_enabled = false
receive_enabled = true
";

/// Every comment the global write did not put there survives it, byte for byte.
///
/// Why: live verification of slice 5 found `PUT /api/channels` deleting a
/// seven-line `tickets-mcp`/ADR-0014 note that has nothing to do with channels.
/// It sat above the `[[listeners]]` table the write removes, and `toml_edit`
/// files a comment block as the following header's leading decor — so removing
/// the table removed the note.
///
/// Pre-fix (`origin/main`) this fails on the first `tickets-mcp` line: the
/// whole block is absent from the rewritten file, together with the heading
/// above `[[channels]]`.
#[test]
fn a_global_write_keeps_a_comment_block_unrelated_to_channels() {
    use super::super::global_channels::{Persisted, channels_in, persist, revision};

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.toml");
    std::fs::write(&path, COMMENT_FIXTURE).expect("seed");

    let stored = channels_in(COMMENT_FIXTURE).expect("the fixture parses");
    let before = revision(&stored).expect("revision");
    let outcome = persist(&path, &before, &stored).expect("the write lands");
    assert!(
        matches!(outcome, Persisted::Written { .. }),
        "the fixture's own list writes back: {outcome:?}"
    );

    let raw = std::fs::read_to_string(&path).expect("read back");
    assert!(
        !raw.contains("[[listeners]]"),
        "the deprecated table is dropped by the write:\n{raw}"
    );
    for line in [
        "# tickets-mcp is intentionally NOT a `driver = \"direct\"` (OpenRPC) endpoint:",
        "# the `tickets-mcp` binary speaks MCP framing, not OpenRPC `rpc.discover`, so a",
        "# direct endpoint would silently fail discovery. It is wired out-of-process via",
        "# the repo-root `.mcp.json` stdio server instead. The former dead `tickets-mcp`",
        "# (and `commons-ticketing`) OpenRPC stubs were retired per ADR-0014 (native",
        "# Rust MCP, PR #2624).",
        "# the stdio services this host spawns",
        "# the harness-wide channels",
        "# trusty-agents global configuration",
    ] {
        assert!(raw.contains(line), "the write dropped `{line}`:\n{raw}");
    }
    assert!(
        raw.contains("[[mcp.services]]") && raw.contains("[[channels]]"),
        "the unrelated table and the published one both survive:\n{raw}"
    );
    // The salvaged block keeps its place: it was above the removed table, so it
    // belongs above whatever followed it, never appended somewhere else.
    let note = raw.find("# tickets-mcp").expect("the note survived");
    let services = raw.find("[[mcp.services]]").expect("services header");
    assert!(note < services, "the note stayed in order:\n{raw}");
}

/// `GET /api/channels` publishes the assistant roster `route_to` may name, and
/// it is the roster the dispatcher measures `route_to` against.
///
/// Why (#7609 slice 7): `route_to` was the one field whose legal values the
/// payload did not carry, so a client had to keep its own roster — and a roster
/// that drifts from this host's turns a legitimate save into an unexplainable
/// 400. The assertion that matters is the SOURCE: the served list is compared
/// against `candidate_agent_names`, the same enumeration
/// `channels::dispatch::unknown_routes` uses on the inbound path, so this can
/// never become a separately maintained list.
///
/// Pre-change this fails on the first assertion: the payload has no
/// `routable_assistants` key at all.
#[tokio::test]
async fn global_channels_serve_the_dispatchable_assistant_roster() {
    let _home_guard = crate::test_env::lock_home();
    let (home, _config) = seed_home();
    for name in ["alpha-assistant", "beta-assistant"] {
        let dir = home.path().join(".trusty-agents/agents").join(name);
        std::fs::create_dir_all(&dir).expect("assistant dir");
        std::fs::write(
            dir.join("agent.toml"),
            format!("[agent]\nname = \"{name}\"\n"),
        )
        .expect("assistant manifest");
    }

    let app = build_router_with_config(AppState::default(), Some(TOKEN.into()));
    let response = app
        .oneshot(get("/api/channels", Some(TOKEN)))
        .await
        .expect("the GET completes");
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let served: Vec<String> = serde_json::from_value(
        body.get("routable_assistants")
            .cloned()
            .expect("the payload carries the roster"),
    )
    .expect("a list of names");

    let dispatchable = crate::listeners::wake::candidate_agent_names()
        .await
        .expect("the dispatcher's roster");
    assert_eq!(
        served, dispatchable,
        "the served roster IS the dispatcher's, not a parallel list"
    );
    for name in ["alpha-assistant", "beta-assistant"] {
        assert!(
            served.iter().any(|value| value == name),
            "`{name}` is routable: {served:?}"
        );
    }

    // The same list decides what dispatch calls unroutable: a `route_to` drawn
    // from it is silent, one outside it is reported.
    let mut routed = crate::channels::Channel {
        id: "global-mail".into(),
        route_to: vec!["alpha-assistant".into()],
        ..Default::default()
    };
    assert!(
        crate::channels::dispatch::unknown_routes(std::slice::from_ref(&routed), &served)
            .is_empty(),
        "a name from the served roster wakes somebody"
    );
    routed.route_to = vec!["gamma-assistant".into()];
    assert_eq!(
        crate::channels::dispatch::unknown_routes(std::slice::from_ref(&routed), &served),
        vec![("global-mail", "gamma-assistant")],
        "a name the roster omits is exactly what dispatch refuses to route"
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
