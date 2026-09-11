//! `GET`/`PUT /api/assistants/:id/mcp` handler tests (#7454).
//!
//! Why: the route is where a per-assistant override becomes durable, so its
//! validation is the half that matters — a name the resolver would silently
//! collapse (a duplicate, or one in both lists) must be refused at the moment
//! it is written, because that is the only point where the user can be told
//! which instruction would have won.
//! What: the three-view read, a round-trip write, the four refusals, the
//! transport-secret redaction both ways, and one full-router test proving the
//! route is wired. Driven against a `tempfile::TempDir`, so nothing races on
//! `$HOME`.
//! Test: This module IS the test.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use trusty_mcp::config::{McpServerConfig, McpTransport};

use crate::api::server::assistant_mcp::{McpBody, read_at, write_at};
use crate::api::server::routes::build_router;
use crate::api::server::state::AppState;
use crate::mcp::shared::GlobalTier;

const ASSISTANT: &str = r#"[agent]
name = "izzie"
role = "assistant"
extends = "assistant"
model = "claude-sonnet-4-6"
description = "test"
"#;

const SECOND: &str = r#"[agent]
name = "cto-assistant"
role = "assistant"
extends = "assistant"
model = "claude-sonnet-4-6"
description = "test"
"#;

/// An agents dir holding both fixtures, and an empty assistants root.
fn fixture() -> (
    tempfile::TempDir,
    Vec<std::path::PathBuf>,
    std::path::PathBuf,
) {
    let tmp = tempfile::tempdir().unwrap();
    let agents = tmp.path().join("agents");
    for (name, body) in [("izzie", ASSISTANT), ("cto-assistant", SECOND)] {
        let package = agents.join(name);
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("agent.toml"), body).unwrap();
    }
    let root = tmp.path().join("homes");
    (tmp, vec![agents], root)
}

fn server(name: &str, command: &str) -> McpServerConfig {
    McpServerConfig::new(
        name,
        McpTransport::Stdio {
            command: command.to_string(),
            args: Vec::new(),
            env: Default::default(),
        },
    )
}

/// A global tier holding one server, so the read has something to layer.
fn tier() -> GlobalTier {
    GlobalTier {
        servers: vec![server("github", "github-mcp")],
        issues: Vec::new(),
        path: std::path::PathBuf::from("servers.toml"),
    }
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn put_body(servers: serde_json::Value, disabled: &[&str]) -> McpBody {
    serde_json::from_value(serde_json::json!({
        "servers": servers,
        "disabled": disabled,
    }))
    .unwrap()
}

/// A body that omits `servers` entirely — what a client editing only the
/// disable list sends, so it never echoes a redacted transport back.
fn disabled_only(disabled: &[&str]) -> McpBody {
    serde_json::from_value(serde_json::json!({ "disabled": disabled })).unwrap()
}

fn stdio_json(name: &str, command: &str) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "enabled": true,
        "transport": {"type": "stdio", "command": command},
    })
}

/// An override is a DELTA, so the pane needs the list it applies to as well as
/// the result. All three views come back from one call.
#[tokio::test]
async fn get_reports_global_overrides_and_resolved() {
    let (_tmp, dirs, root) = fixture();
    let body = body_json(read_at(&dirs, &root, "izzie", tier())).await;

    assert_eq!(body["assistant"], "izzie");
    assert_eq!(body["global"][0]["name"], "github");
    assert_eq!(body["overrides"]["disabled"], serde_json::json!([]));
    assert_eq!(body["resolved"][0]["name"], "github");
    assert_eq!(body["statuses"][0]["usable"], true);
    assert_eq!(body["statuses"][0]["tier"], "global");
    assert_eq!(body["issues"], serde_json::json!([]));
}

#[tokio::test]
async fn put_replaces_the_whole_table() {
    let (_tmp, dirs, root) = fixture();
    let written = body_json(write_at(
        &dirs,
        &root,
        "izzie",
        put_body(
            serde_json::json!([stdio_json("izzie-only", "izzie-bin")]),
            &["github"],
        ),
        tier(),
    ))
    .await;

    assert_eq!(
        written["overrides"]["disabled"],
        serde_json::json!(["github"])
    );
    let resolved: Vec<&str> = written["resolved"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(resolved, ["izzie-only"], "github is disabled for izzie");

    // It is durable, and it belongs to this assistant alone.
    let reread = body_json(read_at(&dirs, &root, "izzie", tier())).await;
    assert_eq!(reread["overrides"]["servers"][0]["name"], "izzie-only");
    let other = body_json(read_at(&dirs, &root, "cto-assistant", tier())).await;
    assert_eq!(other["resolved"][0]["name"], "github");
    assert_eq!(other["overrides"]["servers"], serde_json::json!([]));
}

#[tokio::test]
async fn put_refuses_a_blank_name() {
    let (_tmp, dirs, root) = fixture();
    let response = write_at(
        &dirs,
        &root,
        "izzie",
        put_body(serde_json::json!([stdio_json("   ", "some-bin")]), &[]),
        tier(),
    );
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn put_refuses_a_duplicate_name() {
    let (_tmp, dirs, root) = fixture();
    let response = write_at(
        &dirs,
        &root,
        "izzie",
        put_body(
            serde_json::json!([stdio_json("dup", "a"), stdio_json("dup", "b")]),
            &[],
        ),
        tier(),
    );
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_json(response).await;
    assert!(
        body["error"].as_str().unwrap().contains("dup"),
        "{:?}",
        body["error"]
    );
}

/// The resolver would answer this one (a `Set` after a `Disable` wins), but a
/// user who wrote both probably meant one of them — so they are told now.
#[tokio::test]
async fn put_refuses_a_name_in_both_lists() {
    let (_tmp, dirs, root) = fixture();
    let response = write_at(
        &dirs,
        &root,
        "izzie",
        put_body(
            serde_json::json!([stdio_json("github", "override-bin")]),
            &["github"],
        ),
        tier(),
    );
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

/// A stdio server whose `env` carries a secret, and a remote one whose
/// `headers` carry a bearer token — the two shapes that must never leave here.
fn tier_with_secrets() -> GlobalTier {
    let mut stdio = McpServerConfig::new(
        "github",
        McpTransport::Stdio {
            command: "github-mcp".to_string(),
            args: Vec::new(),
            env: [("API_KEY".to_string(), "secret-value".to_string())]
                .into_iter()
                .collect(),
        },
    );
    stdio.extensions.insert(
        "description".to_string(),
        serde_json::json!("issue tooling"),
    );
    let remote = McpServerConfig::new(
        "remote",
        McpTransport::Http {
            url: "https://mcp.example/api".to_string(),
            headers: [("Authorization".to_string(), "Bearer t".to_string())]
                .into_iter()
                .collect(),
        },
    );
    GlobalTier {
        servers: vec![stdio, remote],
        issues: Vec::new(),
        path: std::path::PathBuf::from("servers.toml"),
    }
}

/// `McpTransport`'s `Serialize` is deliberately unredacted so the config file
/// round-trips, so serialising a resolved server straight into the response
/// would return every inline API key and bearer token over the loopback API.
/// Key NAMES survive; no value does — in any of the three views.
#[tokio::test]
async fn get_redacts_env_and_header_values() {
    let (_tmp, dirs, root) = fixture();
    let response = read_at(&dirs, &root, "izzie", tier_with_secrets());
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .unwrap();
    let raw = String::from_utf8(bytes.to_vec()).unwrap();

    assert!(
        !raw.contains("secret-value"),
        "the stdio server's env value leaked: {raw}"
    );
    assert!(
        !raw.contains("Bearer t"),
        "the remote server's header value leaked: {raw}"
    );

    let body: serde_json::Value = serde_json::from_str(&raw).unwrap();
    for view in ["global", "resolved"] {
        assert_eq!(
            body[view][0]["transport"]["env"]["API_KEY"], "<redacted>",
            "{view}: the key name must survive, marked"
        );
        assert_eq!(
            body[view][1]["transport"]["headers"]["Authorization"], "<redacted>",
            "{view}: the header name must survive, marked"
        );
    }
    // Everything an operator identifies the server by is untouched.
    assert_eq!(body["resolved"][0]["transport"]["command"], "github-mcp");
    assert_eq!(
        body["resolved"][1]["transport"]["url"],
        "https://mcp.example/api"
    );
    assert_eq!(
        body["resolved"][0]["extensions"]["description"],
        "issue tooling"
    );
}

/// The write half of that redaction: a client that edits what the GET rendered
/// sends the marker back, and storing it would replace a working credential
/// with the word that stood in for it.
#[tokio::test]
async fn put_refuses_a_redacted_value() {
    let (_tmp, dirs, root) = fixture();
    let response = write_at(
        &dirs,
        &root,
        "izzie",
        put_body(
            serde_json::json!([{
                "name": "mine",
                "enabled": true,
                "transport": {
                    "type": "stdio",
                    "command": "mine-bin",
                    "env": {"API_KEY": "<redacted>"},
                },
            }]),
            &[],
        ),
        tier(),
    );
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert!(
        body["error"].as_str().unwrap().contains("API_KEY"),
        "the refusal names the value to re-enter: {:?}",
        body["error"]
    );

    // Nothing was written: the assistant still has no overrides.
    let reread = body_json(read_at(&dirs, &root, "izzie", tier())).await;
    assert_eq!(reread["overrides"]["servers"], serde_json::json!([]));
}

/// The route a client editing only the disable list takes. It never restates a
/// transport, so it can never round-trip a marker — and the servers it did not
/// send survive.
#[tokio::test]
async fn put_without_servers_keeps_the_stored_ones() {
    let (_tmp, dirs, root) = fixture();
    write_at(
        &dirs,
        &root,
        "izzie",
        put_body(
            serde_json::json!([stdio_json("izzie-only", "izzie-bin")]),
            &[],
        ),
        tier(),
    );

    let written = body_json(write_at(
        &dirs,
        &root,
        "izzie",
        disabled_only(&["github"]),
        tier(),
    ))
    .await;
    assert_eq!(written["overrides"]["servers"][0]["name"], "izzie-only");
    assert_eq!(
        written["overrides"]["disabled"],
        serde_json::json!(["github"])
    );

    // Sending an explicit empty list still clears them — absence and "none"
    // are different answers.
    let cleared = body_json(write_at(
        &dirs,
        &root,
        "izzie",
        put_body(serde_json::json!([]), &["github"]),
        tier(),
    ))
    .await;
    assert_eq!(cleared["overrides"]["servers"], serde_json::json!([]));
}

/// Validation trims, so storing the untrimmed name would accept `" github"` as
/// a disable of `github` and then match nothing — a dead entry rather than the
/// override the user asked for.
#[tokio::test]
async fn put_trims_a_stored_name() {
    let (_tmp, dirs, root) = fixture();
    let written = body_json(write_at(
        &dirs,
        &root,
        "izzie",
        put_body(
            serde_json::json!([stdio_json("  izzie-only  ", "izzie-bin")]),
            &["  github  "],
        ),
        tier(),
    ))
    .await;

    assert_eq!(written["overrides"]["servers"][0]["name"], "izzie-only");
    assert_eq!(
        written["overrides"]["disabled"],
        serde_json::json!(["github"])
    );
    let resolved: Vec<&str> = written["resolved"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        resolved,
        ["izzie-only"],
        "a trimmed disable actually matches the global server"
    );
}

#[tokio::test]
async fn get_rejects_an_unknown_assistant() {
    let (_tmp, dirs, root) = fixture();
    let response = read_at(&dirs, &root, "ghost", tier());
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// A malformed `[mcp]` table falls back to the global set, and the response
/// names the override file rather than leaving the pane guessing.
#[tokio::test]
async fn get_reports_a_malformed_override_table() {
    let (_tmp, dirs, root) = fixture();
    let home = root.join("izzie");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("config.toml"), "[mcp]\ndisabled = 7\n").unwrap();

    let body = body_json(read_at(&dirs, &root, "izzie", tier())).await;
    assert_eq!(
        body["resolved"][0]["name"], "github",
        "global still applies"
    );
    assert_eq!(body["issues"][0]["tier"], "assistant");
    assert!(
        body["issues"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("config.toml"),
        "{:?}",
        body["issues"][0]["path"]
    );
}

#[tokio::test]
async fn mcp_route_is_wired_into_the_router() {
    let router = build_router(AppState::default());
    let response = router
        .oneshot(
            Request::builder()
                .uri("/api/assistants/definitely-not-an-assistant/mcp")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        response.status(),
        StatusCode::NOT_IMPLEMENTED,
        "the route must exist"
    );
}
