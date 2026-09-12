//! The grant ceiling on a turn-originated settings patch (#7396).
//!
//! Why: `ask_concierge` puts `settings.patch` inside an ordinary model turn, so
//! a turn steered by text the assistant merely READ — an ingested document, an
//! attached table cell, a listener excerpt — could otherwise write its own
//! `[permissions].scopes` and then pass every later `scope_granted` check. The
//! tool's schema has always promised "Role and delegation ceilings cannot be
//! widened"; these tests are what makes that promise checkable.
//! What: drives the real turn path (`assistant_settings::operate_at`, the seam
//! `ConciergeTool::execute` calls) against a fixture assistant, and pins three
//! things — a widening request is refused, the refusal names the offenders, and
//! the manifest on disk is untouched — plus the narrowing that must still work
//! and the operator HTTP route that carries no ceiling at all.
//! Test: This module IS the test.

use crate::api::server::agent_patch::{PatchAgentRequest, patch_agent_at};
use crate::api::server::assistant_settings::{config_revision, operate_at};
use axum::http::StatusCode;
use serde_json::json;
use std::path::PathBuf;

/// A fixture `AgentConfig` can actually load, granting `memory.*` and two tools.
const FIXTURE: &str = r#"[agent]
name = "fixture"
role = "assistant"
runner = "subprocess"
model = "openai/gpt-4o-mini"
description = "Synthetic assistant"

[llm]
temperature = 0.0
max_tokens = 128

[system_prompt]
content = "Synthetic"

[tools]
allow = ["memory_recall", "memory_write"]

[permissions]
scopes = ["memory.*"]
"#;

fn fixture() -> (tempfile::TempDir, Vec<PathBuf>, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("agents");
    std::fs::create_dir(&dir).unwrap();
    let manifest = dir.join("fixture.toml");
    std::fs::write(&manifest, FIXTURE).unwrap();
    (tmp, vec![dir], manifest)
}

fn revision(manifest: &std::path::Path) -> String {
    config_revision(&std::fs::read_to_string(manifest).unwrap())
}

/// THE security test for CRITICAL 1: a model turn cannot grant itself anything.
///
/// Why: the escalation is one tool call — `settings.patch` with a wider
/// `scopes` list — and every later permission decision reads that list back, so
/// a request that is silently narrowed instead of refused would leave the model
/// believing it succeeded while the operator's log shows nothing. Refusing
/// WHOLE, before the write, is what keeps the manifest a true record.
/// What: `scopes:["*"]` and a foreign family (`google.*`) are both refused `400`
/// through the turn path, the body names the field and the offenders, and the
/// file on disk is byte-identical afterwards.
/// Test: this function IS the test.
#[tokio::test]
async fn turn_patch_cannot_widen_its_own_scopes() {
    let (_tmp, dirs, manifest) = fixture();
    let before = std::fs::read_to_string(&manifest).unwrap();

    for widening in [json!(["*"]), json!(["memory.*", "google.*"])] {
        let refusal = operate_at(
            &dirs,
            "fixture",
            "settings.patch",
            "config",
            json!({"revision": revision(&manifest), "scopes": widening}),
        )
        .await
        .expect_err("a turn must not widen its own scopes");
        assert_eq!(refusal.0, StatusCode::BAD_REQUEST, "{refusal:?}");
        let body = refusal.1.0;
        assert_eq!(body["field"], "permissions.scopes", "{body}");
        assert!(
            body["error"].as_str().unwrap().contains("only narrow"),
            "{body}"
        );
        assert!(!body["refused"].as_array().unwrap().is_empty(), "{body}");
        assert_eq!(
            std::fs::read_to_string(&manifest).unwrap(),
            before,
            "a refused grant edit must leave the manifest untouched"
        );
    }
}

/// The same ceiling on the delegation and capability lists, and the narrowing
/// that must keep working.
///
/// Why: a ceiling that also refused narrowing would be a usability regression
/// the next change would be tempted to remove wholesale, and `skills.allow`
/// widens the effective TOOL patterns (`skills::manifest::effective_tool_patterns`)
/// just as surely as `tools.allow` does — checking only `scopes` would leave the
/// escalation reachable one field over.
/// What: a subset of the granted family is accepted and persisted; a tool, skill
/// or sub-agent the manifest does not already grant is refused.
/// Test: this function IS the test.
#[tokio::test]
async fn turn_patch_may_narrow_but_not_widen() {
    let (_tmp, dirs, manifest) = fixture();

    for (section, patch, field) in [
        (
            "config",
            json!({"tools_allow": ["l0_shell_exec"]}),
            "tools.allow",
        ),
        (
            "skills",
            json!({"skills_allow": ["deploy"]}),
            "skills.allow",
        ),
        (
            // On the server-owned reachable floor, so `narrow_to_floor` passes
            // it through and the CEILING is what refuses it — the manifest
            // grants no sub-agent at all.
            "subagents",
            json!({"subagents_delegate_allowed": ["research-agent"]}),
            "subagents.delegate_allowed",
        ),
    ] {
        let mut payload = patch;
        payload["revision"] = json!(revision(&manifest));
        let refusal = operate_at(&dirs, "fixture", "settings.patch", section, payload)
            .await
            .expect_err("a turn widened a grant list the manifest does not hold");
        assert_eq!(refusal.1.0["field"], field, "{:?}", refusal.1.0);
    }

    // `memory.read` is concrete and the manifest already grants `memory.*`, so
    // this is a narrowing and must be persisted exactly.
    let saved = operate_at(
        &dirs,
        "fixture",
        "settings.patch",
        "config",
        json!({"revision": revision(&manifest), "scopes": ["memory.read"]}),
    )
    .await
    .expect("narrowing a granted family is legitimate");
    assert!(saved.is_object(), "{saved}");
    let raw = std::fs::read_to_string(&manifest).unwrap();
    let doc: toml::Value = toml::from_str(&raw).expect("still valid TOML");
    assert_eq!(
        doc["permissions"]["scopes"],
        toml::Value::Array(vec![toml::Value::String("memory.read".into())])
    );
    assert_eq!(
        doc["permissions"]["replace_scopes"],
        toml::Value::Boolean(true)
    );
}

/// The operator HTTP route carries no ceiling — widening there is legitimate.
///
/// Why: the defect was a MODEL TURN granting itself capability, not an operator
/// editing their own assistant in Settings. A ceiling applied to both would have
/// made the Settings pane unable to grant anything, which is why the ceiling
/// travels on the request (`PatchAgentRequest::ceiling`) rather than being read
/// from the file inside the writer.
/// What: the same widening `patch_agent_at` refuses through `operate_at` is
/// accepted when called directly, as the route calls it.
/// Test: this function IS the test.
#[tokio::test]
async fn the_operator_route_still_widens() {
    let (_tmp, dirs, manifest) = fixture();
    let response = patch_agent_at(
        &dirs,
        "fixture",
        PatchAgentRequest {
            scopes: Some(vec!["google.*".into(), "memory.*".into()]),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let raw = std::fs::read_to_string(&manifest).unwrap();
    assert!(raw.contains("google.*"), "{raw}");
}
