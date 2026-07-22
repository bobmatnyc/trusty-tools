//! Tests for `history::allowed_tool_names_for` — the RBAC-tier gate fixing
//! code-critic BLOCK finding 2 (epic #3052 PR B): `chat_with_tools_gated`'s
//! dispatch loop (`registry.dispatch_gated`) enforces only a name allowlist,
//! never `restricted_tiers`, so `run_pm_task_with_history` must compute and
//! pass a tier-scoped `allowed_tools` list itself. Split out of `history.rs`
//! (sibling `_tests.rs` file, this crate's `#[path=...]` pattern) to keep
//! the production file under the 500-SLOC cap — see `history.rs`'s `#[cfg(test)]`
//! declaration.

use serde_json::json;

use super::allowed_tool_names_for;
use crate::rbac::{ServiceTier, UserIdentity};
use crate::tools::ToolRegistry;
use crate::tools::pm_bridge::PmBridgeTool;
use crate::tools::pm_bridge_backend::PmBridgeBackend;

/// Minimal always-succeeding `PmBridgeBackend` — these tests exercise
/// RBAC gating only, never the real subprocess/MCP backends.
struct NoopBackend;

#[async_trait::async_trait]
impl PmBridgeBackend for NoopBackend {
    async fn run(
        &self,
        _route: crate::intent::route::BridgeRoute,
        _task: &str,
    ) -> anyhow::Result<String> {
        Ok("ok".to_string())
    }
}

/// Builds the SAME shape of registry `run_pm_task_with_history` builds
/// for the purposes of this test: `dispatch_task` restricted to
/// `ReadOnly`/`Analytics` (exactly as the production registration
/// above), alongside one unrestricted tool (`add_project`) standing in
/// for the rest of ctrl's ungated tool surface.
fn registry_with_pm_bridge() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(std::sync::Arc::new(
        PmBridgeTool::new(std::sync::Arc::new(NoopBackend))
            .with_restricted_tiers(vec![ServiceTier::ReadOnly, ServiceTier::Analytics]),
    ));
    registry.register(std::sync::Arc::new(super::AddProjectTool));
    registry
}

#[test]
fn allowed_tool_names_for_no_user_returns_none_full_access() {
    let registry = registry_with_pm_bridge();
    assert_eq!(
        allowed_tool_names_for(&registry, None),
        None,
        "local REPL/CLI path (no UserIdentity) must be unaffected — full access, as before this fix"
    );
}

#[test]
fn allowed_tool_names_for_read_only_excludes_dispatch_task() {
    let registry = registry_with_pm_bridge();
    let user = UserIdentity::new("u1", "u1", ServiceTier::ReadOnly);
    let names = allowed_tool_names_for(&registry, Some(&user)).expect("Some for a real user");
    assert!(!names.iter().any(|n| n == "dispatch_task"));
    assert!(names.iter().any(|n| n == "add_project"));
}

#[test]
fn allowed_tool_names_for_analytics_excludes_dispatch_task() {
    let registry = registry_with_pm_bridge();
    let user = UserIdentity::new("u2", "u2", ServiceTier::Analytics);
    let names = allowed_tool_names_for(&registry, Some(&user)).expect("Some for a real user");
    assert!(!names.iter().any(|n| n == "dispatch_task"));
    assert!(names.iter().any(|n| n == "add_project"));
}

#[test]
fn allowed_tool_names_for_all_tier_includes_dispatch_task() {
    let registry = registry_with_pm_bridge();
    let user = UserIdentity::new("u3", "u3", ServiceTier::All);
    let names = allowed_tool_names_for(&registry, Some(&user)).expect("Some for a real user");
    assert!(names.iter().any(|n| n == "dispatch_task"));
}

/// The integration-level proof code-critic asked for: given the REAL
/// registry (with `dispatch_task` restricted exactly as production
/// registers it) and a ReadOnly `UserIdentity`, drive the ACTUAL
/// `ToolRegistry::dispatch_gated` enforcement path — the same call
/// `chat_with_tools_gated`'s dispatch loop makes
/// (`llm/tool_loop/mod.rs:395-396`) — with the `allowed_tools` this fix
/// computes, and assert the call is rejected. This is the mechanism that
/// actually stops invocation; a ReadOnly/Analytics `UserIdentity`
/// reaching this point can never successfully dispatch `dispatch_task`,
/// regardless of what the LLM is told to call.
#[tokio::test]
async fn dispatch_task_not_invocable_for_read_only_user_through_dispatch_gated() {
    let registry = registry_with_pm_bridge();
    let user = UserIdentity::new("u4", "u4", ServiceTier::ReadOnly);
    let allowed = allowed_tool_names_for(&registry, Some(&user));

    let result = registry
        .dispatch_gated(
            "dispatch_task",
            json!({ "task": "spawn a new session" }),
            allowed.as_deref(),
        )
        .await;

    assert!(
        result.is_error(),
        "dispatch_task must be rejected for a ReadOnly caller through the real dispatch path"
    );
    assert!(
        result.content().contains("not permitted"),
        "got: {}",
        result.content()
    );
}

/// Sibling proof for `Analytics`.
#[tokio::test]
async fn dispatch_task_not_invocable_for_analytics_user_through_dispatch_gated() {
    let registry = registry_with_pm_bridge();
    let user = UserIdentity::new("u5", "u5", ServiceTier::Analytics);
    let allowed = allowed_tool_names_for(&registry, Some(&user));

    let result = registry
        .dispatch_gated(
            "dispatch_task",
            json!({ "task": "spawn a new session" }),
            allowed.as_deref(),
        )
        .await;

    assert!(result.is_error());
}

/// The positive case: an `All`-tier caller (or the unauthenticated local
/// REPL/CLI path, `None`) must still be able to invoke `dispatch_task`
/// through the same real dispatch path — this fix must not become a
/// blanket denial.
#[tokio::test]
async fn dispatch_task_invocable_for_all_tier_user_through_dispatch_gated() {
    let registry = registry_with_pm_bridge();
    let user = UserIdentity::new("u6", "u6", ServiceTier::All);
    let allowed = allowed_tool_names_for(&registry, Some(&user));

    let result = registry
        .dispatch_gated(
            "dispatch_task",
            json!({ "task": "spawn a new session" }),
            allowed.as_deref(),
        )
        .await;

    assert!(
        !result.is_error(),
        "All tier must still reach dispatch_task: {}",
        result.content()
    );
}

#[tokio::test]
async fn dispatch_task_invocable_when_no_user_through_dispatch_gated() {
    let registry = registry_with_pm_bridge();
    let allowed = allowed_tool_names_for(&registry, None);

    let result = registry
        .dispatch_gated(
            "dispatch_task",
            json!({ "task": "spawn a new session" }),
            allowed.as_deref(),
        )
        .await;

    assert!(
        !result.is_error(),
        "no UserIdentity (local REPL/CLI) must preserve full access: {}",
        result.content()
    );
}
