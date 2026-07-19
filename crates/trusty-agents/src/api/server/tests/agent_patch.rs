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
        tmp.path(),
        "engineer",
        PatchAgentRequest {
            model_id: Some("anthropic/claude-opus-4-6".to_string()),
            provider_id: None,
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
        tmp.path(),
        "does-not-exist",
        PatchAgentRequest {
            model_id: Some("gpt-4o-mini".to_string()),
            provider_id: None,
        },
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn patch_agent_empty_body_returns_400() {
    let tmp = tempfile::tempdir().unwrap();
    write_fixture(tmp.path(), "engineer", SUBPROCESS_FIXTURE);

    let resp = patch_agent_at(tmp.path(), "engineer", PatchAgentRequest::default()).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn patch_agent_unknown_provider_id_returns_400() {
    let tmp = tempfile::tempdir().unwrap();
    write_fixture(tmp.path(), "engineer", SUBPROCESS_FIXTURE);

    let resp = patch_agent_at(
        tmp.path(),
        "engineer",
        PatchAgentRequest {
            model_id: None,
            provider_id: Some("not-a-real-provider".to_string()),
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
        tmp.path(),
        "pm",
        PatchAgentRequest {
            model_id: Some("openai/gpt-4.1".to_string()),
            provider_id: None,
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
        tmp.path(),
        "pm",
        PatchAgentRequest {
            model_id: Some("gpt-4o-mini".to_string()),
            provider_id: None,
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
        tmp.path(),
        "pm",
        PatchAgentRequest {
            model_id: Some("claude-opus-4-6".to_string()),
            provider_id: None,
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
        tmp.path(),
        "broken",
        PatchAgentRequest {
            model_id: Some("gpt-4o-mini".to_string()),
            provider_id: None,
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
        tmp.path(),
        "pm",
        PatchAgentRequest {
            model_id: Some("claude-opus-4-6".to_string()),
            provider_id: Some("anthropic".to_string()),
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
#[tokio::test]
async fn patch_agent_provider_only_uses_default_model() {
    let tmp = tempfile::tempdir().unwrap();
    write_fixture(tmp.path(), "engineer", SUBPROCESS_FIXTURE);

    let resp = patch_agent_at(
        tmp.path(),
        "engineer",
        PatchAgentRequest {
            model_id: None,
            provider_id: Some("openai".to_string()),
        },
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 4 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let expected_default = trusty_common::inference::registry::capabilities_for("openai")
        .unwrap()
        .default_model;
    assert_eq!(body["model"], expected_default);
    assert_eq!(body["provider_id"], "openai");
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
        tmp.path(),
        "../escape",
        PatchAgentRequest {
            model_id: Some("gpt-4o-mini".to_string()),
            provider_id: None,
        },
    )
    .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
