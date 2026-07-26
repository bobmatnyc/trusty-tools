//! `GET /api/agents/:name/skills` handler tests (#3933).
//!
//! Why: The Phase-1 pane guessed capability from glob prefixes because no route
//! resolved anything. This endpoint's value is entirely in what it *resolves*,
//! so the tests assert resolution — that a granted skill carries its human name
//! and its one tool, that an ungranted one is returned as ungranted rather than
//! omitted, that a dangling `[skills].allow` id is surfaced, and that a pattern
//! the catalog cannot match is reported honestly instead of silently dropped.
//! What: `skills_at` driven against a `tempfile::TempDir` (the `agent_stores`
//! pattern, so tests don't race siblings on cwd/`$HOME`), plus one full-router
//! test proving the route is wired into `build_router`.
//! Test: This module IS the test.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use crate::api::server::agent_skills::skills_at;
use crate::api::server::routes::build_router;
use crate::api::server::state::AppState;

/// Grants via `[tools].allow` only — the pre-#3933 shape every agent uses.
const TOOLS_ONLY_FIXTURE: &str = r#"[agent]
name = "izzie"
role = "assistant"
model = "claude-sonnet-4-6"
description = "test"

[tools]
allow = ["get_train_schedule", "get_weather", "granola_*", "granola_list_meetings"]
"#;

/// Grants via `[skills].allow` only — no `[tools].allow` at all.
const SKILLS_ONLY_FIXTURE: &str = r#"[agent]
name = "skilled"
role = "assistant"
model = "claude-sonnet-4-6"
description = "test"

[skills]
allow = ["mta-train-time", "handoff-protocol", "no-such-skill"]
"#;

/// Declares no capability at all.
const BARE_FIXTURE: &str = r#"[agent]
name = "plain"
role = "assistant"
model = "claude-sonnet-4-6"
description = "test"
"#;

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn find<'a>(body: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    body["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == id)
        .unwrap_or_else(|| panic!("skill {id} missing from response"))
}

#[tokio::test]
async fn skills_route_reports_granted_skills_with_human_names() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("izzie.toml"), TOOLS_ONLY_FIXTURE).unwrap();

    let resp = skills_at(&[dir.path().to_path_buf()], "izzie").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;

    let trains = find(&body, "mta-train-time");
    assert_eq!(trains["granted"], true);
    assert_eq!(trains["name"], "MTA Train Time");
    assert_eq!(trains["tools"], serde_json::json!(["get_train_schedule"]));
    assert_eq!(trains["kind"], "action");
    assert_eq!(trains["origin"]["kind"], "builtin");

    // Ungranted skills are RETURNED as ungranted, not omitted — the Phase-3
    // editor needs the full choice, and "absent" reads as "does not exist".
    assert_eq!(find(&body, "gmail-search")["granted"], false);
    assert_eq!(body["declares_capability"], true);
    // 2 catalog skills + 1 derived card for the exactly-named MCP tool.
    assert_eq!(body["granted_count"], 3);
}

#[tokio::test]
async fn skills_route_derives_a_card_for_an_exactly_named_unknown_tool() {
    // C-04.3 over the wire: an agent granting a live-discovered MCP tool by its
    // exact name must not read as holding no capability. The card is badged
    // `derived` and carries no invented description.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("izzie.toml"), TOOLS_ONLY_FIXTURE).unwrap();

    let body = body_json(skills_at(&[dir.path().to_path_buf()], "izzie").await).await;
    let card = find(&body, "derived:granola_list_meetings");
    assert_eq!(card["granted"], true);
    assert_eq!(card["origin"]["kind"], "derived");
    assert_eq!(card["name"], "Granola List Meetings");
    assert_eq!(card["description"], "");
    assert_eq!(card["tools"], serde_json::json!(["granola_list_meetings"]));
    // The glob beside it names no single tool, so it stays a reported pattern
    // rather than becoming a fabricated card.
    assert_eq!(body["unmatched_patterns"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn skills_route_one_card_per_tool_never_a_family() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("izzie.toml"), TOOLS_ONLY_FIXTURE).unwrap();
    let body = body_json(skills_at(&[dir.path().to_path_buf()], "izzie").await).await;
    for skill in body["skills"].as_array().unwrap() {
        let tools = skill["tools"].as_array().unwrap();
        assert!(
            tools.len() <= 1,
            "{} wraps {} tools; the model is one skill per tool",
            skill["id"],
            tools.len()
        );
    }
}

#[tokio::test]
async fn skills_route_resolves_a_skills_only_agent() {
    // C-04.2 over the wire: `[skills].allow` with no `[tools].allow` grants
    // exactly the wrapped tools, and a tool-less skill is granted by id.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("skilled.toml"), SKILLS_ONLY_FIXTURE).unwrap();

    let body = body_json(skills_at(&[dir.path().to_path_buf()], "skilled").await).await;
    assert_eq!(find(&body, "mta-train-time")["granted"], true);
    assert_eq!(find(&body, "handoff-protocol")["granted"], true);
    assert_eq!(
        find(&body, "handoff-protocol")["tools"],
        serde_json::json!([]),
        "a tool-less skill grants no tools and says so"
    );
    // The tool the *other* MTA skill wraps was never granted.
    assert_eq!(find(&body, "mta-service-alerts")["granted"], false);
}

#[tokio::test]
async fn skills_route_surfaces_unresolved_skill_ids() {
    // S-11: a dangling reference is surfaced, never silently dropped.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("skilled.toml"), SKILLS_ONLY_FIXTURE).unwrap();

    let body = body_json(skills_at(&[dir.path().to_path_buf()], "skilled").await).await;
    let unresolved = body["unresolved"].as_array().unwrap();
    assert_eq!(unresolved.len(), 1);
    assert_eq!(unresolved[0]["id"], "no-such-skill");
    assert!(
        unresolved[0]["reason"]
            .as_str()
            .unwrap()
            .contains("no skill")
    );
}

#[tokio::test]
async fn skills_route_reports_unmatched_patterns_honestly() {
    // `granola_*` comes from a live-discovered MCP service, so the compile-time
    // catalog cannot match it. The route must say that, not imply the grant is
    // broken and not hide it.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("izzie.toml"), TOOLS_ONLY_FIXTURE).unwrap();

    let body = body_json(skills_at(&[dir.path().to_path_buf()], "izzie").await).await;
    let unmatched = body["unmatched_patterns"].as_array().unwrap();
    assert_eq!(unmatched.len(), 1);
    assert_eq!(unmatched[0]["pattern"], "granola_*");
    assert!(
        unmatched[0]["reason"]
            .as_str()
            .unwrap()
            .contains("discovered at dispatch time")
    );
}

#[tokio::test]
async fn skills_route_never_claims_an_unverified_credential() {
    // The honesty rule with teeth: an OAuth-backed provider reports its
    // requirement with `configured: null`, never `true`.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("izzie.toml"), TOOLS_ONLY_FIXTURE).unwrap();

    let body = body_json(skills_at(&[dir.path().to_path_buf()], "izzie").await).await;
    let gmail = find(&body, "gmail-search");
    assert_eq!(gmail["provider"]["provider"], "Google Workspace");
    assert!(gmail["provider"]["configured"].is_null());
    assert!(gmail["provider"]["env_var"].is_null());

    let trains = find(&body, "mta-train-time");
    assert_eq!(trains["provider"]["env_var"], "MTA_API_KEY");
    assert!(trains["provider"]["configured"].is_boolean());
}

#[tokio::test]
async fn skills_route_bare_agent_grants_nothing() {
    // Deny-on-absent, stated: no declaration is not "unrestricted".
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("plain.toml"), BARE_FIXTURE).unwrap();

    let body = body_json(skills_at(&[dir.path().to_path_buf()], "plain").await).await;
    assert_eq!(body["declares_capability"], false);
    assert_eq!(body["granted_count"], 0);
    assert!(
        body["skills"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["granted"] == false)
    );
}

#[tokio::test]
async fn skills_route_unknown_agent_404() {
    let dir = tempfile::tempdir().unwrap();
    let resp = skills_at(&[dir.path().to_path_buf()], "ghost").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn skills_route_invalid_name_400() {
    let dir = tempfile::tempdir().unwrap();
    let resp = skills_at(&[dir.path().to_path_buf()], "../etc").await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn skills_route_degrades_on_malformed_toml() {
    // Same posture as `/stores`: a hand-edited file that breaks the capability
    // tables still renders the rest of the pane.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("broken.toml"), "this is not = = toml").unwrap();

    let resp = skills_at(&[dir.path().to_path_buf()], "broken").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert!(body["config_error"].is_string());
    assert_eq!(body["granted_count"], 0);
}

#[tokio::test]
async fn skills_route_is_wired_into_the_router() {
    // Guards the wiring itself: a handler nobody can reach is not a feature.
    let app = build_router(AppState::default());
    let req = Request::builder()
        .uri("/api/agents/definitely-not-a-real-agent-3933/skills")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "route must resolve to the handler (agent 404), not to a router 404"
    );
    let body = body_json(resp).await;
    assert_eq!(body["error"], "unknown agent");
}
