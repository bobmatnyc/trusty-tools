//! Tests for `history::allowed_tool_names_for` — the RBAC-tier gate fixing
//! code-critic BLOCK finding 2 (epic #3052 PR B): `chat_with_tools_gated`'s
//! dispatch loop (`registry.dispatch_gated`) enforces only a name allowlist,
//! never `restricted_tiers`, so `run_pm_task_with_history` must compute and
//! pass a tier-scoped `allowed_tools` list itself. Split out of `history.rs`
//! (sibling `_tests.rs` file, this crate's `#[path=...]` pattern) to keep
//! the production file under the 500-SLOC cap — see `history.rs`'s `#[cfg(test)]`
//! declaration.

use serde_json::json;

use super::{allowed_tool_names_for, ctrl_delegate_posture};
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
        _target: Option<&str>,
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

// --- `ctrl_delegate_posture` (security fix, item 3, #4126) ---
//
// The THIRD `delegate_to_agent` construction site: `run_pm_task_with_history`
// backs the REPL, Slack, Telegram, and the API server, and `ctrl.toml` (the
// standalone-mode default) declares `role = "assistant"` (#3812) — the same
// role value the OTHER two dispatch paths treat as "holds live
// external-content tools, must never produce an unrestricted
// `delegate_to_agent` spawn." Before this fix, this path applied NEITHER a
// taint NOR a role-gate at all.

#[test]
fn ctrl_delegate_posture_orchestrator_role_is_unrestricted() {
    // `resolve_agent_config` can load a project's `pm.toml` (`role =
    // "orchestrator"`) instead of `ctrl.toml`. That case must be completely
    // unaffected — `pm` is the trusted orchestrator, not a sandboxed
    // persona — so this dispatch path keeps working exactly as before for it.
    let (taint, roles) = ctrl_delegate_posture("orchestrator", None, None);
    assert_eq!(taint, None, "pm/orchestrator must not be tainted");
    assert_eq!(roles, None, "pm/orchestrator must not be role-gated");
}

#[test]
fn ctrl_delegate_posture_assistant_role_fails_closed_without_allow() {
    // `ctrl.toml` declares no `[tools]` section at all — `native_allow` is
    // `None` — and there is no inbound taint (ctrl is always a top-level
    // dispatch, never itself a delegation target). The fix must still
    // produce an EXPLICIT empty (deny-all) taint, not skip tainting.
    let (taint, roles) = ctrl_delegate_posture("assistant", None, None);
    assert_eq!(
        taint,
        Some(Vec::new()),
        "an assistant-tier ctrl with no declared [tools].allow must taint its \
         delegate_to_agent spawns with an explicit deny-all set, never None \
         (which would mean untainted/unrestricted)"
    );
    assert!(
        roles.is_some(),
        "assistant-tier ctrl must always be role-gated"
    );
}

#[test]
fn ctrl_delegate_posture_assistant_role_forwards_native_allow() {
    // When `ctrl.toml` (or an operator override) DOES declare
    // `[tools].allow`, that posture is what gets forwarded as the taint —
    // not silently dropped to empty.
    let native = vec!["web_search".to_string(), "delegate_to_agent".to_string()];
    let (taint, _roles) = ctrl_delegate_posture("assistant", Some(&native), None);
    assert_eq!(taint, Some(native));
}

#[test]
fn ctrl_delegate_posture_assistant_role_always_gates_target_roles() {
    // The role-gate must exclude orchestrator/controller/observer/analysis —
    // the same privilege-escalation targets #3555 closed for the other two
    // dispatch paths — and must include the worker roster ctrl legitimately
    // delegates to.
    let (_taint, roles) = ctrl_delegate_posture("assistant", None, None);
    let roles = roles.expect("assistant-tier ctrl must be role-gated");
    for expected in [
        "engineer",
        "qa",
        "researcher",
        "documentation",
        "ops",
        "planner",
    ] {
        assert!(
            roles.iter().any(|r| r == expected),
            "{expected} must be an allowed delegation target for ctrl, got: {roles:?}"
        );
    }
    assert!(
        !roles.iter().any(|r| r == "orchestrator"),
        "orchestrator (pm) must NOT be a directly delegatable role for the \
         assistant-tier gate, got: {roles:?}"
    );
    assert!(
        !roles.iter().any(|r| r == "controller"),
        "controller must NOT be a directly delegatable role, got: {roles:?}"
    );
}
