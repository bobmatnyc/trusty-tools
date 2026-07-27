//! `GET /api/agents/:name/permissions` handler tests (#3936, DOC-57 §7).
//!
//! Why: This route's whole value is honesty (PM-6) — every field must carry
//! `enforced` truthfully, and `source` must distinguish what an agent
//! declared itself from what it inherited. The tests pin: a declared scope,
//! an inherited scope naming the immediate base, `[permissions].scopes`
//! superseding legacy `[tools].scopes` per CC-9, `user_authority` never
//! inheriting `true` from a base (PM-3), and honest degradation on malformed
//! TOML / an unresolvable `extends` chain.
//! What: `permissions_at` driven against a `tempfile::TempDir` (the
//! `agent_stores`/`agent_skills` pattern), plus one full-router test.
//! Test: This module IS the test.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::api::server::agent_permissions::permissions_at;
use crate::api::server::routes::build_router;
use crate::api::server::state::AppState;

// `AgentConfig::by_name_in` — the full loader `permissions_at` uses to
// resolve `extends` — requires `[llm]` and `[system_prompt]` (no serde
// defaults on `AgentConfig::llm`/`system_prompt`), unlike the sibling
// routes' partial-parse fixtures. Every fixture that needs full-chain
// resolution therefore carries both sections.
const BASE_FIXTURE: &str = r#"[agent]
name = "assistant"
role = "assistant"
model = "claude-sonnet-4-6"
description = "test"

[tools]
scopes = ["memory.read"]

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "test"
"#;

/// Declares its own scope AND extends a base that declares another.
const CHILD_FIXTURE: &str = r#"[agent]
name = "cto-assistant"
role = "assistant"
model = "claude-sonnet-4-6"
description = "test"
extends = "assistant"

[permissions]
scopes = ["google.gmail.*"]
default_tier = "analytics"
autonomy = "learn-to-act"

[[permissions.grants]]
skill = "gmail"
mode = "ask"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "test"
"#;

/// `[permissions].scopes` supersedes legacy `[tools].scopes` in the SAME
/// file (CC-9) — declares both to prove the precedence, not the union.
const CC9_FIXTURE: &str = r#"[agent]
name = "migrated"
role = "assistant"
model = "claude-sonnet-4-6"
description = "test"

[tools]
scopes = ["memory.read"]

[permissions]
scopes = ["search.read"]

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "test"
"#;

/// A base with `user_authority = true` and a child that extends it without
/// declaring its own (PM-3: must never inherit `true`).
const AUTHORITY_BASE_FIXTURE: &str = r#"[agent]
name = "authority-holder"
role = "assistant"
model = "claude-sonnet-4-6"
description = "test"

[permissions]
user_authority = true

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "test"
"#;

const AUTHORITY_CHILD_FIXTURE: &str = r#"[agent]
name = "overlay"
role = "assistant"
model = "claude-sonnet-4-6"
description = "test"
extends = "authority-holder"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "test"
"#;

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn find_scope<'a>(body: &'a serde_json::Value, pattern: &str) -> &'a serde_json::Value {
    body["scopes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["pattern"] == pattern)
        .unwrap_or_else(|| panic!("scope {pattern} missing from response: {body:?}"))
}

#[tokio::test]
async fn permissions_route_reports_declared_and_inherited_scopes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("assistant.toml"), BASE_FIXTURE).unwrap();
    std::fs::write(dir.path().join("cto-assistant.toml"), CHILD_FIXTURE).unwrap();

    let resp = permissions_at(&[dir.path().to_path_buf()], "cto-assistant").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    let own = find_scope(&body, "google.gmail.*");
    assert_eq!(own["source"], "declared");
    assert_eq!(own["enforced"], true);

    let inherited = find_scope(&body, "memory.read");
    assert_eq!(
        inherited["source"], "inherited:assistant",
        "a scope only the base declared must name the immediate base"
    );
    assert_eq!(inherited["enforced"], true);

    assert_eq!(body["tiers"]["default"], "analytics");
    assert_eq!(body["tiers"]["enforced"], false);
    assert_eq!(body["autonomy"]["mode"], "learn-to-act");
    assert_eq!(body["autonomy"]["enforced"], false);

    let grants = body["grants"].as_array().unwrap();
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0]["skill"], "gmail");
    assert_eq!(grants[0]["mode"], "ask");
    assert_eq!(grants[0]["enforced"], false);
}

#[tokio::test]
async fn permissions_route_permissions_scopes_supersedes_legacy_tools_scopes() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("migrated.toml"), CC9_FIXTURE).unwrap();

    let resp = permissions_at(&[dir.path().to_path_buf()], "migrated").await;
    let body = body_json(resp).await;
    let scopes = body["scopes"].as_array().unwrap();
    assert_eq!(scopes.len(), 1, "CC-9: the union is not taken, {scopes:?}");
    assert_eq!(scopes[0]["pattern"], "search.read");
    assert_eq!(scopes[0]["source"], "declared");
}

#[tokio::test]
async fn permissions_route_never_inherits_user_authority() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("authority-holder.toml"),
        AUTHORITY_BASE_FIXTURE,
    )
    .unwrap();
    std::fs::write(dir.path().join("overlay.toml"), AUTHORITY_CHILD_FIXTURE).unwrap();

    let resp = permissions_at(&[dir.path().to_path_buf()], "overlay").await;
    let body = body_json(resp).await;
    assert_eq!(
        body["user_authority"]["value"], false,
        "a child must never inherit user_authority=true from its base (PM-3)"
    );
    assert_eq!(body["user_authority"]["enforced"], false);

    // The base itself still reports its own explicit value.
    let base_resp = permissions_at(&[dir.path().to_path_buf()], "authority-holder").await;
    let base_body = body_json(base_resp).await;
    assert_eq!(base_body["user_authority"]["value"], true);
}

#[tokio::test]
async fn permissions_route_grants_no_permission_it_did_not_already_hold() {
    // C-06.4: the route is read-only and must never widen a scope beyond
    // what the config declares — sanity-check on the bare-agent case.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("plain.toml"),
        "[agent]\nname = \"plain\"\nrole = \"assistant\"\nmodel = \"claude-sonnet-4-6\"\ndescription = \"test\"\n",
    )
    .unwrap();

    let resp = permissions_at(&[dir.path().to_path_buf()], "plain").await;
    let body = body_json(resp).await;
    assert!(body["scopes"].as_array().unwrap().is_empty());
    assert!(body["grants"].as_array().unwrap().is_empty());
    assert_eq!(body["user_authority"]["value"], false);
}

#[tokio::test]
async fn permissions_route_unknown_agent_404() {
    let dir = tempfile::tempdir().unwrap();
    let resp = permissions_at(&[dir.path().to_path_buf()], "nobody").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn permissions_route_rejects_traversal_name() {
    let dir = tempfile::tempdir().unwrap();
    let resp = permissions_at(&[dir.path().to_path_buf()], "../etc").await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn permissions_route_degrades_on_malformed_toml() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("broken.toml"), "not = = toml").unwrap();

    let resp = permissions_at(&[dir.path().to_path_buf()], "broken").await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a hand-edit typo must not 500 the panel"
    );
    let body = body_json(resp).await;
    assert!(body["scopes"].as_array().unwrap().is_empty());
    assert!(body["config_error"].is_string());
}

#[tokio::test]
async fn permissions_route_degrades_gracefully_on_unresolvable_extends() {
    // `extends` points at a base that does not exist — `AgentConfig::by_name_in`
    // fails to resolve the chain, and the route must still render this
    // agent's OWN declarations rather than going dark.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("orphan.toml"),
        "[agent]\nname = \"orphan\"\nrole = \"assistant\"\nmodel = \"claude-sonnet-4-6\"\ndescription = \"test\"\nextends = \"does-not-exist\"\n\n[permissions]\nscopes = [\"memory.read\"]\n",
    )
    .unwrap();

    let resp = permissions_at(&[dir.path().to_path_buf()], "orphan").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let scopes = body["scopes"].as_array().unwrap();
    assert_eq!(scopes.len(), 1);
    assert_eq!(scopes[0]["pattern"], "memory.read");
    assert_eq!(scopes[0]["source"], "declared");
    assert!(body["extends_warning"].is_string());
}

/// Proves the route is wired into `build_router`.
#[tokio::test]
async fn permissions_route_is_wired_into_router() {
    let app: axum::Router = build_router(AppState::default());
    let req = Request::builder()
        .uri("/api/agents/definitely-not-an-agent-3936/permissions")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = body_json(resp).await;
    assert_eq!(
        body["error"], "unknown agent",
        "a 404 from the handler, not from an unrouted path"
    );
}
