//! `PATCH /api/agents/:name` handler tests (#3246).
//!
//! Why: The endpoint's contract is "persist to disk and round-trip" — a
//! response that looks right but didn't actually write, or that clobbered
//! unrelated TOML content, would be worse than an obvious failure. These
//! tests drive `patch_agent_at` directly against a `tempfile::TempDir`
//! (mirroring the `scan_agents_dir`/`load_sessions_from` pattern in
//! `listing.rs`) so they don't race sibling tests on the process cwd, plus
//! one full-router test proving the route is actually wired into
//! `build_router`.
//! What: Persistence + round-trip, unknown-agent 404, empty-body 400,
//! unknown-provider 400, the claude-code/non-Anthropic runner conflict via
//! an explicit prefixed model (rejection) and an explicit `provider_id`
//! (acceptance), the same conflict via a *bare* unprefixed model_id (the
//! fail-shut/no-fail-open case, #3287 review), the positive counterpart
//! where a bare model_id is accepted because an on-disk `provider_id`
//! already resolves to Anthropic, provider-only defaulting, malformed
//! on-disk TOML (500, not a panic), and preservation of unrelated TOML
//! content (comments + untouched keys).
//! Test: This module IS the test.

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use tower::ServiceExt;

use crate::api::server::agent_patch::{PatchAgentRequest, patch_agent_at};
use crate::api::server::projects::parse_agent_toml;
use crate::api::server::routes::build_router;
use crate::api::server::state::AppState;

const SUBPROCESS_FIXTURE: &str = r#"[agent]
name = "engineer"
role = "engineer"
runner = "subprocess"
model = "openai/gpt-4o-mini"
description = "A test agent" # trailing comment must survive

[llm]
temperature = 0.2
max_tokens = 2048
"#;

const CLAUDE_CODE_FIXTURE: &str = r#"[agent]
name = "pm"
role = "orchestrator"
runner = "claude-code"
model = "claude-sonnet-4-6"
description = "PM persona"
"#;

/// Same as [`CLAUDE_CODE_FIXTURE`] but with `provider_id = "anthropic"`
/// already persisted from a prior patch — the on-disk-fallback source in
/// `agent_patch.rs:180-189`.
const CLAUDE_CODE_WITH_PROVIDER_FIXTURE: &str = r#"[agent]
name = "pm"
role = "orchestrator"
runner = "claude-code"
model = "claude-sonnet-4-6"
provider_id = "anthropic"
description = "PM persona"
"#;

/// Deliberately unparseable TOML (unterminated `[agent` table header) — the
/// #3246 write path must surface this as a clean `500`, not a panic.
const MALFORMED_TOML_FIXTURE: &str = "[agent\nname = \"broken\n";

fn write_fixture(dir: &std::path::Path, name: &str, contents: &str) {
    std::fs::write(dir.join(format!("{name}.toml")), contents).unwrap();
}

#[tokio::test]
async fn patch_agent_persists_model_and_round_trips() {
    let tmp = tempfile::tempdir().unwrap();
    write_fixture(tmp.path(), "engineer", SUBPROCESS_FIXTURE);

    let resp = patch_agent_at(
        &[tmp.path().to_path_buf()],
        "engineer",
        PatchAgentRequest {
            model_id: Some("anthropic/claude-opus-4-6".to_string()),
            provider_id: None,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), 8 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["name"], "engineer");
    assert_eq!(body["model"], "anthropic/claude-opus-4-6");
    // Unrelated fields must survive untouched in the response...
    assert_eq!(body["role"], "engineer");
    assert_eq!(body["description"], "A test agent");

    // ...and, independently, on disk: re-read + re-parse the file directly
    // (not just trusting the handler's own response) to prove the write
    // actually landed and round-trips.
    let on_disk = std::fs::read_to_string(tmp.path().join("engineer.toml")).unwrap();
    assert!(
        on_disk.contains("model = \"anthropic/claude-opus-4-6\""),
        "on-disk model not updated: {on_disk}"
    );
    assert!(
        on_disk.contains("# trailing comment must survive"),
        "comment lost on write: {on_disk}"
    );
    assert!(
        on_disk.contains("temperature = 0.2"),
        "unrelated [llm] table lost on write: {on_disk}"
    );
    let reparsed = parse_agent_toml(&on_disk, "engineer").expect("still valid TOML");
    assert_eq!(reparsed["model"], "anthropic/claude-opus-4-6");
    assert_eq!(reparsed["description"], "A test agent");
}

#[tokio::test]
async fn patch_agent_unknown_agent_returns_404() {
    let tmp = tempfile::tempdir().unwrap();
    let resp = patch_agent_at(
        &[tmp.path().to_path_buf()],
        "does-not-exist",
        PatchAgentRequest {
            model_id: Some("gpt-4o-mini".to_string()),
            provider_id: None,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn patch_agent_empty_body_returns_400() {
    let tmp = tempfile::tempdir().unwrap();
    write_fixture(tmp.path(), "engineer", SUBPROCESS_FIXTURE);

    let resp = patch_agent_at(
        &[tmp.path().to_path_buf()],
        "engineer",
        PatchAgentRequest::default(),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn patch_agent_unknown_provider_id_returns_400() {
    let tmp = tempfile::tempdir().unwrap();
    write_fixture(tmp.path(), "engineer", SUBPROCESS_FIXTURE);

    let resp = patch_agent_at(
        &[tmp.path().to_path_buf()],
        "engineer",
        PatchAgentRequest {
            model_id: None,
            provider_id: Some("not-a-real-provider".to_string()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("unknown provider_id")
    );
    // Must not have written anything.
    let on_disk = std::fs::read_to_string(tmp.path().join("engineer.toml")).unwrap();
    assert_eq!(on_disk, SUBPROCESS_FIXTURE);
}

/// Why: The concrete rejection case the issue calls out — a claude-code
/// agent (spawns the local `claude` CLI, which only talks to Anthropic)
/// must not silently accept an OpenAI-prefixed model.
#[tokio::test]
async fn patch_agent_claude_code_rejects_non_anthropic_model() {
    let tmp = tempfile::tempdir().unwrap();
    write_fixture(tmp.path(), "pm", CLAUDE_CODE_FIXTURE);

    let resp = patch_agent_at(
        &[tmp.path().to_path_buf()],
        "pm",
        PatchAgentRequest {
            model_id: Some("openai/gpt-4.1".to_string()),
            provider_id: None,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("claude-code")
    );
    let on_disk = std::fs::read_to_string(tmp.path().join("pm.toml")).unwrap();
    assert_eq!(on_disk, CLAUDE_CODE_FIXTURE, "rejected write must not land");
}

/// Why: #3287 review (HIGH) — a *bare*, unprefixed `model_id` (no
/// `provider/` prefix, no explicit `provider_id`) previously resolved to
/// `None` and the runner-conflict check silently skipped it (fail-open),
/// so e.g. `{"model_id":"gpt-4o-mini"}` against a claude-code agent would
/// return `200 OK` and persist a model the claude-code runner can never
/// dispatch. The fix makes an unresolved provider for a claude-code agent
/// just as rejected as an explicitly wrong one (fail-shut).
/// What: PATCHes a bare, non-Anthropic-looking model_id with no provider_id
/// against the claude-code fixture; asserts `400` and that the file on disk
/// is byte-identical to the original fixture (no partial/silent write).
#[tokio::test]
async fn patch_agent_claude_code_rejects_bare_non_anthropic_model() {
    let tmp = tempfile::tempdir().unwrap();
    write_fixture(tmp.path(), "pm", CLAUDE_CODE_FIXTURE);

    let resp = patch_agent_at(
        &[tmp.path().to_path_buf()],
        "pm",
        PatchAgentRequest {
            model_id: Some("gpt-4o-mini".to_string()),
            provider_id: None,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::BAD_REQUEST,
        "a bare, unresolvable model_id must be rejected, not silently accepted"
    );
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("claude-code"),
        "error should explain the claude-code constraint: {body}"
    );
    let on_disk = std::fs::read_to_string(tmp.path().join("pm.toml")).unwrap();
    assert_eq!(
        on_disk, CLAUDE_CODE_FIXTURE,
        "rejected write must not land on disk"
    );
}

/// Why: The on-disk `provider_id` fallback (`agent_patch.rs:180-189`) exists
/// so a follow-up patch that only changes `model_id` doesn't have to repeat
/// a `provider_id` a prior patch already established. This is the positive
/// counterpart to `patch_agent_claude_code_rejects_bare_non_anthropic_model`:
/// the same bare, unprefixed `model_id` with no `provider_id` in the
/// request must be ACCEPTED when the file already carries
/// `provider_id = "anthropic"`, because that on-disk value resolves the
/// provider unambiguously to Anthropic.
/// What: PATCHes a bare Anthropic model_id (no provider_id in the request)
/// against [`CLAUDE_CODE_WITH_PROVIDER_FIXTURE`]; asserts `200` and that the
/// new model persisted (both in the response and on disk).
#[tokio::test]
async fn patch_agent_claude_code_model_only_inherits_ondisk_anthropic_provider() {
    let tmp = tempfile::tempdir().unwrap();
    write_fixture(tmp.path(), "pm", CLAUDE_CODE_WITH_PROVIDER_FIXTURE);

    let resp = patch_agent_at(
        &[tmp.path().to_path_buf()],
        "pm",
        PatchAgentRequest {
            model_id: Some("claude-opus-4-6".to_string()),
            provider_id: None,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "a bare model_id must be accepted when an on-disk provider_id already resolves to Anthropic"
    );
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["model"], "claude-opus-4-6");
    assert_eq!(body["provider_id"], "anthropic");

    let on_disk = std::fs::read_to_string(tmp.path().join("pm.toml")).unwrap();
    assert!(
        on_disk.contains("model = \"claude-opus-4-6\""),
        "on-disk model not updated: {on_disk}"
    );
    assert!(
        on_disk.contains("provider_id = \"anthropic\""),
        "on-disk provider_id should still be present: {on_disk}"
    );
}

/// Why: `raw.parse::<DocumentMut>()` on an already-corrupt on-disk file must
/// surface as a clean `500`, not a panic that takes the whole server down.
/// What: Writes deliberately unparseable TOML, PATCHes it, asserts `500`.
#[tokio::test]
async fn patch_agent_malformed_toml_returns_500() {
    let tmp = tempfile::tempdir().unwrap();
    write_fixture(tmp.path(), "broken", MALFORMED_TOML_FIXTURE);

    let resp = patch_agent_at(
        &[tmp.path().to_path_buf()],
        "broken",
        PatchAgentRequest {
            model_id: Some("gpt-4o-mini".to_string()),
            provider_id: None,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("not valid TOML")
    );
    let on_disk = std::fs::read_to_string(tmp.path().join("broken.toml")).unwrap();
    assert_eq!(on_disk, MALFORMED_TOML_FIXTURE, "must not touch the file");
}

/// Why: Same claude-code agent, but an explicit `provider_id: "anthropic"`
/// with a bare model slug must be accepted — this is the happy path the
/// rejection test above is guarding.
#[tokio::test]
async fn patch_agent_claude_code_accepts_anthropic_provider() {
    let tmp = tempfile::tempdir().unwrap();
    write_fixture(tmp.path(), "pm", CLAUDE_CODE_FIXTURE);

    let resp = patch_agent_at(
        &[tmp.path().to_path_buf()],
        "pm",
        PatchAgentRequest {
            model_id: Some("claude-opus-4-6".to_string()),
            provider_id: Some("anthropic".to_string()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["model"], "claude-opus-4-6");
    assert_eq!(body["provider_id"], "anthropic");
}

/// Why: A caller that only wants to switch provider (without hand-typing a
/// model slug) should get that provider's registry default model.
///
/// #3765 changed the provider used here from `openai` to `atlascloud`:
/// `provider_id` is no longer an inert key, so the write gate now refuses a
/// provider the agent loader would refuse to pin, and `openai` is one of
/// those (see `patch_agent_rejects_a_provider_dispatch_cannot_reach`).
#[tokio::test]
async fn patch_agent_provider_only_uses_default_model() {
    let tmp = tempfile::tempdir().unwrap();
    write_fixture(tmp.path(), "engineer", SUBPROCESS_FIXTURE);

    let resp = patch_agent_at(
        &[tmp.path().to_path_buf()],
        "engineer",
        PatchAgentRequest {
            model_id: None,
            provider_id: Some("atlascloud".to_string()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let expected_default = trusty_common::inference::registry::capabilities_for("atlascloud")
        .unwrap()
        .default_model;
    assert_eq!(body["model"], expected_default);
    assert_eq!(body["provider_id"], "atlascloud");
}

/// Why (#3765): `provider_id` used to be written verbatim and read by nothing,
/// so persisting `openai` was harmless. Now the loader PINS the agent to it
/// and fails closed, which would turn a successful-looking GUI write into an
/// agent that cannot load. The write gate must refuse the same providers the
/// loader refuses — `openai` and `together` still fall through to OpenRouter
/// with a bare, unroutable slug.
/// Test: itself.
#[tokio::test]
async fn patch_agent_rejects_a_provider_dispatch_cannot_reach() {
    let tmp = tempfile::tempdir().unwrap();
    write_fixture(tmp.path(), "engineer", SUBPROCESS_FIXTURE);

    for provider in ["openai", "together"] {
        let resp = patch_agent_at(
            &[tmp.path().to_path_buf()],
            "engineer",
            PatchAgentRequest {
                model_id: None,
                provider_id: Some(provider.to_string()),
                ..Default::default()
            },
        )
        .await;
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "{provider} must be refused"
        );
        let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let err = body["error"].as_str().unwrap_or_default();
        assert!(err.contains(provider), "error must name it: {err}");
        assert!(err.contains("cannot dispatch"), "{err}");
    }
}

/// Why: `PATCH /api/agents/:name` must actually be registered on the router
/// (not just callable as a bare function) — an unknown agent through the
/// full HTTP stack should 404, not fall through to the SPA catch-all (which
/// would return HTML, not JSON, and a 200).
#[tokio::test]
async fn patch_agent_route_is_wired_into_router() {
    let app: Router = build_router(AppState::default());
    let req = Request::builder()
        .method(Method::PATCH)
        .uri("/api/agents/this-agent-does-not-exist-3246")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"model_id":"gpt-4o-mini"}"#))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"], "unknown agent");
}

#[tokio::test]
async fn patch_agent_invalid_name_returns_400() {
    let tmp = tempfile::tempdir().unwrap();
    let resp = patch_agent_at(
        &[tmp.path().to_path_buf()],
        "../escape",
        PatchAgentRequest {
            model_id: Some("gpt-4o-mini".to_string()),
            provider_id: None,
            ..Default::default()
        },
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// #3738: `GET /api/agents` must surface `[agent].display_name` so the GUI's
/// per-message speaker attribution reads the persona's human-facing label
/// ("CTO Bot") straight from the catalog — and must fall back to `name` when
/// no `display_name` is declared, matching `AgentInfo::display_label`.
#[test]
fn parse_agent_toml_surfaces_display_name() {
    let with_display = r#"
[agent]
name = "cto-assistant"
role = "assistant"
display_name = "CTO Bot"
"#;
    let parsed = parse_agent_toml(with_display, "cto-assistant").expect("valid TOML");
    assert_eq!(parsed["display_name"], "CTO Bot");

    // No display_name → falls back to name (never "").
    let without_display = r#"
[agent]
name = "engineer"
role = "engineer"
"#;
    let parsed = parse_agent_toml(without_display, "engineer").expect("valid TOML");
    assert_eq!(parsed["display_name"], "engineer");
}

// ---------------------------------------------------------------------------
// #3819: package-vs-flat path resolution, tools_allow, personality, and the
// new GET /api/agents/:name and GET /api/agents/:name/persona reads.
// ---------------------------------------------------------------------------

use crate::api::server::agent_patch::{get_agent_at, persona_at};

fn write_package(dir: &std::path::Path, name: &str, agent_toml: &str, persona_md: &str) {
    let pkg = dir.join(name);
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(pkg.join("agent.toml"), agent_toml).unwrap();
    std::fs::write(pkg.join("persona.md"), persona_md).unwrap();
}

const PACKAGE_AGENT_TOML: &str = r#"[agent]
name = "izzie"
role = "assistant"
runner = "subprocess"
model = "anthropic/claude-sonnet-4-6"
description = "Personal assistant"
"#;

/// A package (`izzie/agent.toml`) takes precedence over a same-named flat
/// shadow (`izzie.toml`) — matching `scan_agents_dir_tiered`'s dispatch
/// resolution order. Pre-#3819 this endpoint wrote the flat shadow
/// unconditionally, silently missing the file that actually dispatches.
#[tokio::test]
async fn patch_agent_prefers_package_over_flat_shadow() {
    let tmp = tempfile::tempdir().unwrap();
    write_package(
        tmp.path(),
        "izzie",
        PACKAGE_AGENT_TOML,
        "Original persona.\n",
    );
    // A flat shadow with a DIFFERENT model — if the endpoint wrote here
    // instead, the assertion below on the package file would fail.
    write_fixture(
        tmp.path(),
        "izzie",
        "[agent]\nname = \"izzie\"\nmodel = \"stale-shadow\"\n",
    );

    let resp = patch_agent_at(
        &[tmp.path().to_path_buf()],
        "izzie",
        PatchAgentRequest {
            model_id: Some("anthropic/claude-opus-4-6".to_string()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let package_toml = std::fs::read_to_string(tmp.path().join("izzie/agent.toml")).unwrap();
    assert!(package_toml.contains("claude-opus-4-6"));
    let shadow_toml = std::fs::read_to_string(tmp.path().join("izzie.toml")).unwrap();
    assert!(
        shadow_toml.contains("stale-shadow"),
        "flat shadow must be left untouched"
    );
}

#[tokio::test]
async fn patch_agent_tools_allow_round_trips() {
    let tmp = tempfile::tempdir().unwrap();
    write_package(
        tmp.path(),
        "izzie",
        PACKAGE_AGENT_TOML,
        "Original persona.\n",
    );

    let resp = patch_agent_at(
        &[tmp.path().to_path_buf()],
        "izzie",
        PatchAgentRequest {
            tools_allow: Some(vec!["gworkspace_*".to_string(), "memory_*".to_string()]),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 8 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        body["tools_allow"],
        serde_json::json!(["gworkspace_*", "memory_*"])
    );

    // And a fresh GET reflects the same persisted value.
    let get_resp = get_agent_at(&[tmp.path().to_path_buf()], "izzie").await;
    assert_eq!(get_resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(get_resp.into_body(), 8 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        body["tools_allow"],
        serde_json::json!(["gworkspace_*", "memory_*"])
    );
}

#[tokio::test]
async fn patch_agent_personality_writes_persona_md_for_package_agent() {
    let tmp = tempfile::tempdir().unwrap();
    write_package(
        tmp.path(),
        "izzie",
        PACKAGE_AGENT_TOML,
        "Original persona.\n",
    );

    let resp = patch_agent_at(
        &[tmp.path().to_path_buf()],
        "izzie",
        PatchAgentRequest {
            personality: Some("Warm and witty, Masa-bound persona.".to_string()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let persona = std::fs::read_to_string(tmp.path().join("izzie/persona.md")).unwrap();
    assert_eq!(persona, "Warm and witty, Masa-bound persona.");
}

#[tokio::test]
async fn patch_agent_personality_rejected_for_flat_only_agent() {
    let tmp = tempfile::tempdir().unwrap();
    write_fixture(tmp.path(), "engineer", SUBPROCESS_FIXTURE);

    let resp = patch_agent_at(
        &[tmp.path().to_path_buf()],
        "engineer",
        PatchAgentRequest {
            personality: Some("New prose".to_string()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    // Nothing should have been written for a rejected request.
    assert!(!tmp.path().join("engineer/persona.md").exists());
}

#[tokio::test]
async fn get_agent_persona_reads_package_persona() {
    let tmp = tempfile::tempdir().unwrap();
    write_package(
        tmp.path(),
        "izzie",
        PACKAGE_AGENT_TOML,
        "Hello, I'm Izzie.\n",
    );

    let resp = persona_at(&[tmp.path().to_path_buf()], "izzie").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 8 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["content"], "Hello, I'm Izzie.\n");
    assert_eq!(body["editable"], true);
}

#[tokio::test]
async fn get_agent_persona_not_editable_for_flat_agent() {
    let tmp = tempfile::tempdir().unwrap();
    write_fixture(tmp.path(), "engineer", SUBPROCESS_FIXTURE);

    let resp = persona_at(&[tmp.path().to_path_buf()], "engineer").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 8 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["content"], serde_json::Value::Null);
    assert_eq!(body["editable"], false);
}

#[tokio::test]
async fn get_agent_persona_unknown_agent_404() {
    let tmp = tempfile::tempdir().unwrap();
    let resp = persona_at(&[tmp.path().to_path_buf()], "nope").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn get_agent_route_returns_full_config_for_ctrl_style_agent() {
    // Regression guard for the #3812-role-filter scenario: GET /api/agents/:name
    // must work for a `role = "controller"` package agent even though the
    // LIST route may filter it out of the picker.
    let tmp = tempfile::tempdir().unwrap();
    write_package(
        tmp.path(),
        "ctrl",
        "[agent]\nname = \"ctrl\"\nrole = \"controller\"\nmodel = \"ollama/qwen3:30b\"\n",
        "ctrl persona\n",
    );
    let resp = get_agent_at(&[tmp.path().to_path_buf()], "ctrl").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 8 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["name"], "ctrl");
    assert_eq!(body["role"], "controller");
}

#[tokio::test]
async fn get_agent_route_unknown_agent_404() {
    let tmp = tempfile::tempdir().unwrap();
    let resp = get_agent_at(&[tmp.path().to_path_buf()], "nope").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// Regression guard: `tools_allow`/`scopes` must be read from `[tools]`, NOT
/// `[agent]` — confirmed against `.trusty-agents/agents/izzie/agent.toml`'s
/// real layout (`scopes = [...]` sits inside its `[tools]` table).
#[test]
fn parse_agent_toml_reads_tools_allow_and_scopes_from_tools_table() {
    let toml = r#"
[agent]
name = "izzie"
role = "assistant"

[tools]
allow = ["gworkspace_*", "memory_*"]
scopes = ["memory.read", "google.*"]
"#;
    let parsed = parse_agent_toml(toml, "izzie").expect("valid TOML");
    assert_eq!(
        parsed["tools_allow"],
        serde_json::json!(["gworkspace_*", "memory_*"])
    );
    assert_eq!(
        parsed["scopes"],
        serde_json::json!(["memory.read", "google.*"])
    );
}

/// #3232: `[tools].search_indexes` is part of the agent's declared capability
/// surface, so the catalog reports it next to `tools_allow`/`scopes` — same
/// `[tools]`-table source and same empty-array-not-absent convention, so a
/// client rendering "what can this agent reach" never re-reads the TOML.
#[test]
fn parse_agent_toml_reads_search_indexes_from_tools_table() {
    let toml = r#"
[agent]
name = "cto-assistant"
role = "assistant"

[tools]
allow = ["vector_search"]
search_indexes = ["apex", "cto-projects"]
"#;
    let parsed = parse_agent_toml(toml, "cto-assistant").expect("valid TOML");
    assert_eq!(
        parsed["search_indexes"],
        serde_json::json!(["apex", "cto-projects"])
    );
    // Absent → empty array, never a missing key.
    let bare = parse_agent_toml("[agent]\nname = \"a\"\n", "a").expect("valid TOML");
    assert_eq!(bare["search_indexes"], serde_json::json!([]));
}

#[test]
fn parse_agent_toml_hidden_defaults_false_and_parses_true() {
    let visible = "[agent]\nname = \"a\"\n";
    assert_eq!(parse_agent_toml(visible, "a").unwrap()["hidden"], false);

    let hidden = "[agent]\nname = \"a\"\nhidden = true\n";
    assert_eq!(parse_agent_toml(hidden, "a").unwrap()["hidden"], true);
}

/// Bob's roster-typing directive (#3819): `kind` defaults to `"assistant"`
/// when absent (every pre-existing agent TOML keeps working unchanged) and
/// parses an explicit `"system-tool"` (set on Concierge/`ctrl`).
#[test]
fn parse_agent_toml_kind_defaults_assistant_and_parses_system_tool() {
    let default_kind = "[agent]\nname = \"izzie\"\n";
    assert_eq!(
        parse_agent_toml(default_kind, "izzie").unwrap()["kind"],
        "assistant"
    );

    let system_tool = "[agent]\nname = \"ctrl\"\nkind = \"system-tool\"\n";
    assert_eq!(
        parse_agent_toml(system_tool, "ctrl").unwrap()["kind"],
        "system-tool"
    );
}

/// Regression guard for the demo-build fix (#3819): `GET/PATCH
/// /api/agents/:name` must search MULTIPLE candidate directories (project-
/// local tier, then `$HOME/.trusty-agents/agents`), not a single cwd-
/// relative directory — critical for the packaged desktop app, where the
/// Tauri sidecar's cwd is `/` and every bundled agent lives in the `$HOME`
/// tier. A single-tier lookup would 404 on every bundled agent there.
#[tokio::test]
async fn get_agent_searches_second_tier_when_first_misses() {
    let primary = tempfile::tempdir().unwrap(); // empty — simulates cwd `/`
    let home = tempfile::tempdir().unwrap();
    write_package(home.path(), "izzie", PACKAGE_AGENT_TOML, "Hi, I'm Izzie.\n");

    let dirs = vec![primary.path().to_path_buf(), home.path().to_path_buf()];
    let resp = get_agent_at(&dirs, "izzie").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 8 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["name"], "izzie");
}

/// Project-local tier wins over `$HOME` when both define the same name —
/// same precedence `crate::agents::agents_dir_candidates()` documents.
#[tokio::test]
async fn patch_agent_prefers_first_tier_over_second() {
    let primary = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    write_package(primary.path(), "izzie", PACKAGE_AGENT_TOML, "Primary.\n");
    write_package(home.path(), "izzie", PACKAGE_AGENT_TOML, "Home.\n");

    let dirs = vec![primary.path().to_path_buf(), home.path().to_path_buf()];
    let resp = patch_agent_at(
        &dirs,
        "izzie",
        PatchAgentRequest {
            model_id: Some("anthropic/claude-opus-4-6".to_string()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let primary_toml = std::fs::read_to_string(primary.path().join("izzie/agent.toml")).unwrap();
    assert!(primary_toml.contains("claude-opus-4-6"));
    let home_toml = std::fs::read_to_string(home.path().join("izzie/agent.toml")).unwrap();
    assert!(
        !home_toml.contains("claude-opus-4-6"),
        "the $HOME tier's copy must be untouched when the project-local tier already resolves"
    );
}

// --- ADR-0024 decision 4 sub-answer (b): the server-side write floor ---
//
// The owner ratified that a GUI write "must not be able to widen an assistant's
// reachable set past the floor", and named `tools_allow` above as the precedent
// NOT to copy. These three pin the resulting asymmetry: this field validates,
// that one does not.

/// A write that NARROWS is persisted and round-trips.
///
/// Why: the fail-closed test below would pass against an endpoint that rejected
/// everything. This is the non-vacuity half — the feature has to actually work.
/// What: one floor member is written to `[subagents].delegate_allowed`, and the
/// file on disk carries it.
/// Test: this function IS the test.
#[tokio::test]
async fn patch_agent_writes_a_narrowed_subagent_whitelist() {
    let tmp = tempfile::tempdir().unwrap();
    write_package(
        tmp.path(),
        "izzie",
        PACKAGE_AGENT_TOML,
        "Original persona.\n",
    );

    let resp = patch_agent_at(
        &[tmp.path().to_path_buf()],
        "izzie",
        PatchAgentRequest {
            subagents_delegate_allowed: Some(vec!["research-agent".to_string()]),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Read the written table back through a plain `toml::Value` — the fixture
    // is a minimal `[agent]`-only file that `AgentConfig` (which requires
    // `[llm]`/`[system_prompt]`) would reject for reasons unrelated to this
    // field.
    let raw = std::fs::read_to_string(tmp.path().join("izzie").join("agent.toml")).unwrap();
    let doc: toml::Value = toml::from_str(&raw).expect("still valid TOML");
    assert_eq!(
        doc["subagents"]["delegate_allowed"],
        toml::Value::Array(vec![toml::Value::String("research-agent".to_string())])
    );
}

/// THE security test for decision 4 sub-answer (b): a write may NOT widen the
/// reachable set past the server-owned floor.
///
/// Why: "editable" means a GUI, a script, or anyone who can reach the API can
/// set this value. Following the `tools_allow` precedent literally would let
/// such a caller add `engineer` — or `pm` — to an assistant's reachable set with
/// no server-side check, which the ADR calls out as "a materially different, and
/// weaker, security posture" than the mechanism this one parallels. The request
/// must be refused WHOLE: a partial application would persist a config the
/// caller did not ask for and quietly disagree with the response.
/// What: a mixed list (one legal, two illegal) is rejected `400`, every offender
/// is named back, and — the part that matters — NOTHING is written: the file on
/// disk still declares no whitelist.
/// Test: this function IS the test.
#[tokio::test]
async fn patch_agent_rejects_a_subagent_whitelist_that_widens_past_the_floor() {
    let tmp = tempfile::tempdir().unwrap();
    write_package(
        tmp.path(),
        "izzie",
        PACKAGE_AGENT_TOML,
        "Original persona.\n",
    );
    let before = std::fs::read_to_string(tmp.path().join("izzie").join("agent.toml")).unwrap();

    let resp = patch_agent_at(
        &[tmp.path().to_path_buf()],
        "izzie",
        PatchAgentRequest {
            subagents_delegate_allowed: Some(vec![
                "research-agent".to_string(),
                "engineer".to_string(),
                "pm".to_string(),
            ]),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let bytes = axum::body::to_bytes(resp.into_body(), 8 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let err = body["error"].as_str().unwrap();
    assert!(err.contains("engineer"), "{err}");
    assert!(err.contains("pm"), "{err}");
    assert!(err.contains("narrow"), "{err}");

    let after = std::fs::read_to_string(tmp.path().join("izzie").join("agent.toml")).unwrap();
    assert_eq!(
        before, after,
        "a refused write must leave the file byte-identical — no partial application"
    );
}

/// An EMPTY list is a legitimate narrowing ("reach nothing") and is written as
/// an empty array, not omitted.
///
/// Why: the caller must be able to express "revoke everything" and see it
/// persisted, distinguishably from "never set" — the same distinction
/// `tools_allow` documents for its own empty case. Silently ignoring an empty
/// list would leave a revocation that appeared to succeed and did not.
/// What: `[]` round-trips as an empty `delegate_allowed` array.
/// Test: this function IS the test.
#[tokio::test]
async fn patch_agent_subagent_whitelist_accepts_an_empty_list() {
    let tmp = tempfile::tempdir().unwrap();
    write_package(
        tmp.path(),
        "izzie",
        PACKAGE_AGENT_TOML,
        "Original persona.\n",
    );

    let resp = patch_agent_at(
        &[tmp.path().to_path_buf()],
        "izzie",
        PatchAgentRequest {
            subagents_delegate_allowed: Some(Vec::new()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let raw = std::fs::read_to_string(tmp.path().join("izzie").join("agent.toml")).unwrap();
    let doc: toml::Value = toml::from_str(&raw).expect("still valid TOML");
    assert_eq!(
        doc["subagents"]["delegate_allowed"],
        toml::Value::Array(Vec::new()),
        "an explicit empty list must persist as an empty array, not be omitted"
    );
}
