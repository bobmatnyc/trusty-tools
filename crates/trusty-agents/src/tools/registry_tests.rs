//! Unit tests for the `ToolRegistry` and native tool helpers.
//!
//! Why: Extracted from `tools/mod.rs` (#361) to keep the registry source file
//! under the 500-line cap. The test body is unchanged; only its location
//! moved.
//! What: Exercises register/dispatch/schema/RBAC paths plus the
//! `native_tool_registry` factory.
//! Test: This *is* the test module — run via `cargo test -p trusty-agents`.

use super::*;
use async_trait::async_trait;

struct FakeTool;

#[async_trait]
impl ToolExecutor for FakeTool {
    fn name(&self) -> &str {
        "fake"
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "fake",
                "description": "A fake tool for tests.",
                "parameters": {"type":"object","properties":{},"additionalProperties":false}
            }
        })
    }
    async fn execute(&self, _args: Value) -> ToolResult {
        ToolResult::ok("fake-output")
    }
}

struct FailingTool;

#[async_trait]
impl ToolExecutor for FailingTool {
    fn name(&self) -> &str {
        "fails"
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "fails",
                "description": "Always errors.",
                "parameters": {"type":"object","properties":{},"additionalProperties":false}
            }
        })
    }
    async fn execute(&self, _args: Value) -> ToolResult {
        ToolResult::err("boom")
    }
}

#[tokio::test]
async fn registry_registers_and_dispatches() {
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(FakeTool));
    assert!(reg.contains("fake"));
    let out = reg.dispatch("fake", serde_json::json!({})).await;
    assert!(!out.is_error());
    assert_eq!(out.content(), "fake-output");
}

#[tokio::test]
async fn registry_dispatch_unknown_errors() {
    let reg = ToolRegistry::new();
    let out = reg.dispatch("missing", serde_json::json!({})).await;
    assert!(out.is_error());
    assert!(out.content().contains("missing"));
}

#[tokio::test]
async fn registry_dispatch_propagates_tool_error() {
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(FailingTool));
    let out = reg.dispatch("fails", serde_json::json!({})).await;
    assert!(out.is_error());
    assert!(!out.is_fatal());
    assert_eq!(out.content(), "boom");
}

#[tokio::test]
async fn dispatch_gated_rejects_disallowed() {
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(FakeTool));
    let allowed = vec!["other_tool".to_string()];
    let out = reg
        .dispatch_gated("fake", serde_json::json!({}), Some(&allowed))
        .await;
    assert!(out.is_error());
    assert!(out.content().contains("not permitted"));
    assert!(out.content().contains("other_tool"));
}

#[tokio::test]
async fn dispatch_gated_allows_when_unrestricted() {
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(FakeTool));
    let out = reg
        .dispatch_gated("fake", serde_json::json!({}), None)
        .await;
    assert!(!out.is_error());
    assert_eq!(out.content(), "fake-output");
}

#[tokio::test]
async fn dispatch_gated_allows_when_in_list() {
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(FakeTool));
    let allowed = vec!["fake".to_string(), "other".to_string()];
    let out = reg
        .dispatch_gated("fake", serde_json::json!({}), Some(&allowed))
        .await;
    assert!(!out.is_error());
}

#[test]
fn registers_fs_reader_tools() {
    // #34: the convenience helper should register all three read-only
    // filesystem exploration tools.
    let mut reg = ToolRegistry::new();
    reg.with_fs_reader_tools();
    assert!(reg.contains("read_file"));
    assert!(reg.contains("list_dir"));
    assert!(reg.contains("grep_files"));
}

#[test]
fn registry_schemas_includes_registered_tools() {
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(FakeTool));
    let schemas = reg.schemas();
    assert_eq!(schemas.len(), 1);
    assert_eq!(schemas[0]["function"]["name"], "fake");
}

#[tokio::test]
#[should_panic(expected = "duplicate tool registration")]
#[cfg(debug_assertions)]
async fn register_panics_on_duplicate_name_in_debug() {
    // #101 (MIN-5): registering two tools with the same `name()` must
    // fire the debug_assert so collisions are caught during development.
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(FakeTool));
    reg.register(Arc::new(FakeTool));
}

#[test]
fn delegate_tool_schema_is_valid() {
    let tool = delegate_to_agent_tool().unwrap();
    assert_eq!(tool.function.name, "delegate_to_agent");
    let params = tool.function.parameters.expect("parameters present");
    assert_eq!(params["type"], "object");
    let required = params["required"]
        .as_array()
        .expect("required is array")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(required.contains(&"agent_name".to_string()));
    assert!(required.contains(&"task".to_string()));
}

/// RBAC-restricted fake tool used by `dispatch_for_user_*` and
/// `filter_tools_for_user_*`. Refuses `ReadOnly` callers.
struct RestrictedTool;

#[async_trait]
impl ToolExecutor for RestrictedTool {
    fn name(&self) -> &str {
        "restricted"
    }
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "restricted",
                "description": "Blocked for ReadOnly tier.",
                "parameters": {"type":"object","properties":{},"additionalProperties":false}
            }
        })
    }
    async fn execute(&self, _args: Value) -> ToolResult {
        ToolResult::ok("restricted-output")
    }
    fn restricted_tiers(&self) -> &[crate::rbac::ServiceTier] {
        const TIERS: [crate::rbac::ServiceTier; 1] = [crate::rbac::ServiceTier::ReadOnly];
        &TIERS
    }
}

#[tokio::test]
async fn dispatch_for_user_denies_restricted_tier() {
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(RestrictedTool));
    let user = crate::rbac::UserIdentity::new("u", "u", crate::rbac::ServiceTier::ReadOnly);
    let out = reg
        .dispatch_for_user("restricted", serde_json::json!({}), None, &user)
        .await;
    assert!(out.is_error());
    assert_eq!(
        out.content(),
        "This tool is not available for your access tier."
    );
}

#[tokio::test]
async fn dispatch_for_user_allows_permitted_tier() {
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(RestrictedTool));
    let user = crate::rbac::UserIdentity::new("u", "u", crate::rbac::ServiceTier::All);
    let out = reg
        .dispatch_for_user("restricted", serde_json::json!({}), None, &user)
        .await;
    assert!(!out.is_error());
    assert_eq!(out.content(), "restricted-output");
}

#[tokio::test]
async fn dispatch_for_user_still_honors_allowlist() {
    // RBAC + per-agent allowlist are independent gates; if the allowlist
    // excludes a tool the RBAC check passing doesn't override it.
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(FakeTool));
    let user = crate::rbac::UserIdentity::default();
    let allowed = vec!["other".to_string()];
    let out = reg
        .dispatch_for_user("fake", serde_json::json!({}), Some(&allowed), &user)
        .await;
    assert!(out.is_error());
    assert!(out.content().contains("not permitted"));
}

#[tokio::test]
async fn dispatch_for_user_unknown_tool_returns_no_tool_error() {
    // Unknown tool path: no RBAC check (nothing to check against), the
    // dispatch_gated layer surfaces the "no tool" error.
    let reg = ToolRegistry::new();
    let user = crate::rbac::UserIdentity::default();
    let out = reg
        .dispatch_for_user("ghost", serde_json::json!({}), None, &user)
        .await;
    assert!(out.is_error());
    assert!(out.content().contains("ghost"));
}

#[test]
fn filter_tools_for_user_drops_restricted() {
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(FakeTool));
    reg.register(Arc::new(RestrictedTool));

    let read_only = crate::rbac::UserIdentity::new("u", "u", crate::rbac::ServiceTier::ReadOnly);
    let filtered = reg.filter_tools_for_user(&read_only);
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();
    assert!(names.contains(&"fake"));
    assert!(
        !names.contains(&"restricted"),
        "restricted tool must be hidden from read_only user"
    );
}

#[test]
fn filter_tools_for_user_keeps_all_for_unrestricted() {
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(FakeTool));
    reg.register(Arc::new(RestrictedTool));

    let admin = crate::rbac::UserIdentity::default(); // tier = All
    let filtered = reg.filter_tools_for_user(&admin);
    assert_eq!(filtered.len(), 2);
}

#[test]
fn filter_tools_for_user_empty_registry() {
    let reg = ToolRegistry::new();
    let user = crate::rbac::UserIdentity::default();
    assert!(reg.filter_tools_for_user(&user).is_empty());
}

#[test]
fn native_tool_registry_returns_six_tools_without_ticketing() {
    // Default backends (all None) still returns the six native tools in
    // graceful-degradation mode.
    let tools = native_tool_registry(None, NativeToolBackends::default());
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert_eq!(tools.len(), 6);
    assert!(names.contains(&"search_code"));
    assert!(names.contains(&"search_memory"));
    assert!(names.contains(&"search_skills"));
    assert!(names.contains(&"store_memory"));
    assert!(names.contains(&"retrieve_memory"));
    assert!(names.contains(&"list_memory_keys"));
}
