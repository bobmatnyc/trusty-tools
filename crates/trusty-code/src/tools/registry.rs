//! Tool registry — HashMap dispatcher + schema emitter.
//!
//! Why: Replaces hardcoded `if name == "delegate_to_agent"` dispatch with a
//! polymorphic registry so new tools (`web_search`, `load_skill`, etc.) plug in
//! without touching the PM loop. This is the SOA seam.
//! What: `ToolRegistry` holds `Arc<dyn ToolExecutor>` keyed by name; emits a
//! vector of JSON schemas for the LLM request; dispatches named tool calls to
//! the right executor. RBAC-gated dispatch via `dispatch_for_user`.
//! Test: Unit tests construct a registry with a `MockTool` and assert that
//! `dispatch("mock")` returns the mock's output and `schemas()` contains the
//! mock's schema.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;

use crate::rbac::UserIdentity;
use crate::tools::traits::{ToolExecutor, ToolResult};

/// Registry of tools available to an LLM session.
///
/// Why: Centralizes tool lookup and schema emission so the PM loop and
/// multi-turn sub-agent loop stay short and test-friendly.
/// What: `HashMap<String, Arc<dyn ToolExecutor>>` with `register`, `dispatch`,
/// `dispatch_gated`, `dispatch_for_user`, `schemas`, and `filter_tools_for_user`.
/// Test: See unit tests below.
#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn ToolExecutor>>,
}

impl ToolRegistry {
    /// Construct an empty registry.
    ///
    /// Why: Callers build up the registry incrementally via `register`.
    /// What: Returns a `ToolRegistry` with no tools registered.
    /// Test: `new_registry_is_empty`.
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool by its `name()`.
    ///
    /// Why: Using the tool's own name prevents mismatches between registered
    /// key and schema-declared function name.
    /// What: Inserts into the map. A `debug_assert!` panics in debug builds on
    /// duplicate names so collisions surface during development.
    /// Test: `register_then_dispatch_succeeds`, `register_panics_on_duplicate_in_debug`.
    pub fn register(&mut self, tool: Arc<dyn ToolExecutor>) {
        let name = tool.name().to_string();
        debug_assert!(
            !self.tools.contains_key(&name),
            "duplicate tool registration for '{name}' — collisions overwrite and cause dispatch bugs"
        );
        self.tools.insert(name, tool);
    }

    /// Whether a tool with the given name is registered.
    ///
    /// Why: Lets callers skip registration when a tool already exists.
    /// What: HashMap membership check.
    /// Test: `contains_after_register`.
    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// Dispatch a named tool call.
    ///
    /// Why: Single place where missing-tool errors are surfaced as a structured
    /// `ToolResult::Error` so the LLM loop can continue.
    /// What: Looks up `name`, calls `execute(args)`, returns the `ToolResult`.
    /// Test: Dispatch to an unregistered name returns `ToolResult::Error`;
    /// dispatch to a registered one returns `ToolResult::Success`.
    pub async fn dispatch(&self, name: &str, args: Value) -> ToolResult {
        let Some(tool) = self.tools.get(name) else {
            return ToolResult::err(format!("no tool registered with name '{name}'"));
        };
        tool.execute(args).await
    }

    /// Dispatch with an optional per-agent allowlist.
    ///
    /// Why: Per-agent tool gating prevents, e.g., the plan-agent from shelling
    /// out or the research-agent from delegating. Allowlist is checked
    /// centrally so every call site inherits the same policy.
    /// What: If `allowed` is `Some` and does not contain `name`, returns a
    /// recoverable `ToolResult::Error` naming what is permitted. Otherwise
    /// delegates to `dispatch`.
    /// Test: `dispatch_gated_rejects_disallowed`.
    pub async fn dispatch_gated(
        &self,
        name: &str,
        args: Value,
        allowed: Option<&[String]>,
    ) -> ToolResult {
        if let Some(list) = allowed
            && !list.iter().any(|a| a == name)
        {
            // #2857: a policy gate, not a model mistake — the agent's config
            // decided this, so the model cannot self-correct into it. Logged
            // so an over-tight allowlist is diagnosable from stderr rather
            // than from wondering why an agent never uses a tool.
            tracing::warn!(
                tool = name,
                "per-agent allowlist rejected tool '{name}': not in this agent's permitted set \
                 ({}) — the call was refused without executing",
                list.join(", "),
            );
            return ToolResult::err(format!(
                "Tool '{name}' is not permitted for this agent. Allowed: {}",
                list.join(", ")
            ));
        }
        self.dispatch(name, args).await
    }

    /// Dispatch honoring a `UserIdentity`'s `ServiceTier`.
    ///
    /// Why: Some tools must be denied to less-trusted transports (Slack guest
    /// users, unauthenticated HTTP) even if the LLM tries to call them.
    /// Enforcing the tier check at the dispatch boundary means transport authors
    /// can't accidentally bypass RBAC by forgetting to pre-filter the tool list.
    /// What: If the tool is registered AND `user.can_access_tier` is `false`,
    /// returns `ToolResult::Error`. Otherwise delegates to `dispatch_gated`.
    /// Test: `dispatch_for_user_blocks_restricted_tier`.
    pub async fn dispatch_for_user(
        &self,
        name: &str,
        args: Value,
        allowed: Option<&[String]>,
        user: &UserIdentity,
    ) -> ToolResult {
        if let Some(tool) = self.tools.get(name)
            && !user.can_access_tier(tool.restricted_tiers())
        {
            // #2857: an RBAC denial is a security decision and must always
            // leave an audit trail. Without this, a run where an untrusted
            // transport repeatedly probed restricted tools is
            // indistinguishable from one where it never tried.
            tracing::warn!(
                tool = name,
                tier = ?user.tier,
                "RBAC denied tool '{name}': the caller's service tier is not permitted to \
                 access it — the call was refused without executing"
            );
            return ToolResult::err("This tool is not available for your access tier.");
        }
        self.dispatch_gated(name, args, allowed).await
    }

    /// Build a new registry containing only the tools named in `allowed`.
    ///
    /// Why: The agent loop calls `dispatch_gated(name, args, None)` — it does
    /// not thread a per-agent allowlist through every dispatch. The robust place
    /// to enforce an agent's `tools.allowed` (issue #1029 acceptance criterion)
    /// is therefore *before* the loop runs: hand the loop a registry that simply
    /// does not contain the denied tools, so a denied call surfaces as the same
    /// structured "no tool registered" error and the model can self-correct.
    /// Unknown names in `allowed` are ignored (they register nothing); this keeps
    /// a typo in an agent config from being a hard error.
    /// What: Returns a fresh `ToolRegistry` whose map holds cloned `Arc`s for
    /// exactly the registered tools whose `name()` appears in `allowed`,
    /// preserving the shared executors (no re-construction). Order-independent.
    /// Test: `gated_keeps_only_allowed`, `gated_ignores_unknown_names`.
    pub fn gated(&self, allowed: &[String]) -> ToolRegistry {
        let tools = self
            .tools
            .iter()
            .filter(|(name, _)| allowed.iter().any(|a| a == *name))
            .map(|(name, tool)| (name.clone(), Arc::clone(tool)))
            .collect();
        ToolRegistry { tools }
    }

    /// Emit all registered tool schemas as raw JSON values.
    ///
    /// Why: The sub-agent tool-calling loop attaches raw JSON schemas to LLM
    /// requests; the OpenAI-compatible schema is produced by each tool.
    /// What: Collects `tool.schema()` for every registered tool.
    /// Test: Registering two tools returns a vector of length 2.
    pub fn schemas(&self) -> Vec<Value> {
        self.tools.values().map(|t| t.schema()).collect()
    }

    /// Return the raw schema for a single registered tool by name.
    ///
    /// Why: `llm::ToolCallExtractor` (#1023) validates one tool call's
    /// arguments at a time; looking up exactly one schema is cheaper and
    /// simpler than enumerating and filtering the full `schemas()` vector on
    /// every dispatch.
    /// What: Returns `Some(tool.schema())` for a registered `name`, else `None`.
    /// Test: `schema_for_returns_registered_tool_schema`, `schema_for_none_when_missing`.
    pub fn schema_for(&self, name: &str) -> Option<Value> {
        self.tools.get(name).map(|t| t.schema())
    }

    /// Return the subset of registered tools that `user` may invoke.
    ///
    /// Why: Schema emission for the LLM request must reflect what the user can
    /// actually call, otherwise the model will hallucinate denied calls.
    /// What: Returns cloned `Arc`s for tools where `user.can_access_tier` is true.
    /// Test: `filter_tools_for_user_drops_restricted`.
    pub fn filter_tools_for_user(&self, user: &UserIdentity) -> Vec<Arc<dyn ToolExecutor>> {
        self.tools
            .values()
            .filter(|t| user.can_access_tier(t.restricted_tiers()))
            .cloned()
            .collect()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use serde_json::{Value, json};

    use super::ToolRegistry;
    use crate::rbac::{ServiceTier, UserIdentity};
    use crate::tools::traits::{ToolExecutor, ToolResult};

    /// Minimal mock tool for registry tests.
    struct MockTool {
        name: &'static str,
        restricted: Vec<ServiceTier>,
    }

    impl MockTool {
        fn new(name: &'static str) -> Self {
            Self {
                name,
                restricted: vec![],
            }
        }

        fn restricted(mut self, tier: ServiceTier) -> Self {
            self.restricted.push(tier);
            self
        }
    }

    #[async_trait]
    impl ToolExecutor for MockTool {
        fn name(&self) -> &str {
            self.name
        }

        fn schema(&self) -> Value {
            json!({"type": "function", "function": {"name": self.name, "description": "mock"}})
        }

        async fn execute(&self, _args: Value) -> ToolResult {
            ToolResult::ok(format!("mock-{} executed", self.name))
        }

        fn restricted_tiers(&self) -> &[ServiceTier] {
            &self.restricted
        }
    }

    /// A new registry has no tools.
    ///
    /// Why: Guard the baseline.
    /// What: `new()` + `contains("any")` → false.
    /// Test: This test.
    #[test]
    fn new_registry_is_empty() {
        let reg = ToolRegistry::new();
        assert!(!reg.contains("anything"));
        assert!(reg.schemas().is_empty());
    }

    /// Registering a tool makes it findable.
    ///
    /// Why: Verify the basic register+contains contract.
    /// What: Register "mock", then `contains("mock")` is true.
    /// Test: This test.
    #[test]
    fn contains_after_register() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(MockTool::new("mock")));
        assert!(reg.contains("mock"));
    }

    /// Dispatch to an unknown tool returns a structured error.
    ///
    /// Why: The LLM loop must not panic or abort on a missing tool.
    /// What: `dispatch("unknown", {})` returns `ToolResult::Error`.
    /// Test: This test.
    #[tokio::test]
    async fn dispatch_unknown_returns_error() {
        let reg = ToolRegistry::new();
        let result = reg.dispatch("unknown", json!({})).await;
        assert!(result.is_error());
        assert!(result.content().contains("no tool registered"));
    }

    /// Dispatch to a registered tool returns its output.
    ///
    /// Why: Verify the happy-path dispatch contract.
    /// What: Register "mock", dispatch to it, assert success content.
    /// Test: This test.
    #[tokio::test]
    async fn dispatch_registered_tool_succeeds() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(MockTool::new("mock")));
        let result = reg.dispatch("mock", json!({})).await;
        assert!(!result.is_error());
        assert!(result.content().contains("mock-mock executed"));
    }

    /// `schemas()` returns one entry per registered tool.
    ///
    /// Why: Verify schema emission count.
    /// What: Register two tools; `schemas()` returns 2 items.
    /// Test: This test.
    #[test]
    fn schemas_returns_one_per_tool() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(MockTool::new("tool_a")));
        reg.register(Arc::new(MockTool::new("tool_b")));
        assert_eq!(reg.schemas().len(), 2);
    }

    /// `schema_for` returns the registered tool's schema.
    ///
    /// Why: Guard the direct single-tool lookup used by `ToolCallExtractor`.
    /// What: Register "mock", fetch its schema by name, assert its shape.
    /// Test: this test.
    #[test]
    fn schema_for_returns_registered_tool_schema() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(MockTool::new("mock")));
        let schema = reg.schema_for("mock").expect("schema present");
        assert_eq!(schema["function"]["name"], "mock");
    }

    /// `schema_for` returns `None` for an unregistered name.
    ///
    /// Why: Guard the miss path — no panic, no default schema fabricated.
    /// What: Query a name that was never registered.
    /// Test: this test.
    #[test]
    fn schema_for_none_when_missing() {
        let reg = ToolRegistry::new();
        assert!(reg.schema_for("does_not_exist").is_none());
    }

    /// `dispatch_gated` rejects a tool not in the allowlist.
    ///
    /// Why: Per-agent allowlists must be enforced at dispatch.
    /// What: Register "mock", call `dispatch_gated("mock", {}, Some(&["other"]))`,
    /// expect error.
    /// Test: This test.
    #[tokio::test]
    async fn dispatch_gated_rejects_disallowed() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(MockTool::new("mock")));
        let allowed = vec!["other".to_string()];
        let result = reg.dispatch_gated("mock", json!({}), Some(&allowed)).await;
        assert!(result.is_error());
        assert!(result.content().contains("not permitted"));
    }

    /// `dispatch_gated` with `allowed = None` passes through.
    ///
    /// Why: `None` allowlist means no per-agent restriction.
    /// What: `dispatch_gated("mock", {}, None)` calls through to dispatch.
    /// Test: This test.
    #[tokio::test]
    async fn dispatch_gated_none_allowlist_permits() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(MockTool::new("mock")));
        let result = reg.dispatch_gated("mock", json!({}), None).await;
        assert!(!result.is_error());
    }

    /// `dispatch_for_user` blocks a restricted tier.
    ///
    /// Why: RBAC must be enforced at dispatch.
    /// What: Register a tool restricted to `[ReadOnly]`, dispatch as a
    /// `ReadOnly` user, expect error.
    /// Test: This test.
    #[tokio::test]
    async fn dispatch_for_user_blocks_restricted_tier() {
        let mut reg = ToolRegistry::new();
        let tool = MockTool::new("secret_tool").restricted(ServiceTier::ReadOnly);
        reg.register(Arc::new(tool));

        let user = UserIdentity::new("u1", "alice", ServiceTier::ReadOnly);
        let result = reg
            .dispatch_for_user("secret_tool", json!({}), None, &user)
            .await;
        assert!(result.is_error());
        assert!(result.content().contains("access tier"));
    }

    /// `dispatch_for_user` allows an unrestricted tier.
    ///
    /// Why: `All` tier must pass unrestricted tools.
    /// What: Register an unrestricted tool, dispatch as `All`, expect success.
    /// Test: This test.
    #[tokio::test]
    async fn dispatch_for_user_allows_unrestricted() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(MockTool::new("open_tool")));

        let user = UserIdentity::default(); // tier = All
        let result = reg
            .dispatch_for_user("open_tool", json!({}), None, &user)
            .await;
        assert!(!result.is_error());
    }

    /// `gated` retains only the tools named in the allowlist.
    ///
    /// Why: This is the enforcement point for an agent's `tools.allowed`; the
    /// resulting registry must contain exactly the permitted tools.
    /// What: Register three tools, gate to two of them, assert membership.
    /// Test: This test.
    #[test]
    fn gated_keeps_only_allowed() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(MockTool::new("read_file")));
        reg.register(Arc::new(MockTool::new("write_file")));
        reg.register(Arc::new(MockTool::new("bash")));

        let allowed = vec!["read_file".to_string(), "bash".to_string()];
        let gated = reg.gated(&allowed);

        assert!(gated.contains("read_file"), "allowed tool must survive");
        assert!(gated.contains("bash"), "allowed tool must survive");
        assert!(
            !gated.contains("write_file"),
            "denied tool must be dropped from the gated registry"
        );
        assert_eq!(gated.schemas().len(), 2, "exactly the two allowed tools");
    }

    /// `gated` silently ignores allowlist entries that match no registered tool.
    ///
    /// Why: A typo in an agent's `tools.allowed` should not be a hard error; it
    /// simply contributes no tool to the gated registry.
    /// What: Gate with a mix of a real and a bogus name; only the real one survives.
    /// Test: This test.
    #[test]
    fn gated_ignores_unknown_names() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(MockTool::new("read_file")));

        let allowed = vec!["read_file".to_string(), "does_not_exist".to_string()];
        let gated = reg.gated(&allowed);

        assert!(gated.contains("read_file"));
        assert_eq!(gated.schemas().len(), 1, "unknown name registers nothing");
    }

    /// `gated` with an empty allowlist produces an empty registry.
    ///
    /// Why: An agent that explicitly allows no tools must be handed a registry
    /// with no tools (the model then has nothing to call).
    /// What: Gate a populated registry with `&[]`; assert empty.
    /// Test: This test.
    #[test]
    fn gated_empty_allowlist_is_empty() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(MockTool::new("read_file")));
        let gated = reg.gated(&[]);
        assert!(gated.schemas().is_empty());
        assert!(!gated.contains("read_file"));
    }

    /// `filter_tools_for_user` drops tools that the user's tier is blocked for.
    ///
    /// Why: LLM should not see tools it cannot call.
    /// What: Register one restricted and one open tool; filter for `ReadOnly`.
    /// Expected: only the open tool survives.
    /// Test: This test.
    #[test]
    fn filter_tools_for_user_drops_restricted() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(MockTool::new("open")));
        reg.register(Arc::new(
            MockTool::new("restricted").restricted(ServiceTier::ReadOnly),
        ));

        let user = UserIdentity::new("u", "u", ServiceTier::ReadOnly);
        let filtered = reg.filter_tools_for_user(&user);
        assert_eq!(filtered.len(), 1, "only the open tool should survive");
        assert_eq!(filtered[0].name(), "open");
    }
}
