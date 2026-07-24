//! Tests for `persona` — split out to keep `persona.rs` under the 500-line
//! production SLOC hard cap enforced by `scripts/check_line_cap.sh`.
//!
//! Why: the cap counts SLOC by filename, not by `#[cfg(test)]`, so an inline
//! `mod tests { ... }` inside `persona.rs` counted against its production
//! budget. Moved verbatim (behavior unchanged) following the same
//! `builder.rs`/`builder_tests.rs` split pattern already used elsewhere in
//! the workspace (see `trusty-agents-common::agents::builder_tests`,
//! `trusty-agents::tools::registry_tests`).
//! What: all unit tests for `filter_persona_tool_names`,
//! `persona_allowed_tools`, and `run_pm_task_with_persona`'s fast-fail path.
//! Test: This module IS the test coverage — see `persona.rs`'s doc comments
//! for which test pins which behavior.

use super::*;

/// #3223: `run_pm_task_with_persona` now resolves the agent via
/// `AgentConfig::by_name_async` instead of a hand-rolled `.toml`-only
/// lookup. A name that exists in neither the project dir nor the
/// `$HOME` tier must still fail fast with a "not found"-shaped error —
/// and critically, fail BEFORE any credential resolution or network/LLM
/// call (the `by_name_async` call is the very first thing this function
/// does), so this test needs no API keys/credentials and stays fast.
#[tokio::test]
async fn run_pm_task_with_persona_errs_for_unknown_agent() {
    let project_dir = std::env::temp_dir().join(format!(
        "t3223-persona-missing-{}-{}",
        std::process::id(),
        "run_pm_task_with_persona_errs_for_unknown_agent"
    ));
    let result = run_pm_task_with_persona(
        &project_dir,
        "definitely-not-a-real-agent-3223",
        "hello",
        &[],
        None,
        SessionOverrides::default(),
    )
    .await;
    assert!(
        result.is_err(),
        "unknown persona must error, not silently fall back to some other agent"
    );
}

/// #3285: with a persona-typical `allow` list (globs alongside a couple
/// of exact names), only names matching a pattern survive — mirrors the
/// glob semantics `match_any_glob` implements and pins the first half of
/// the allow/scope gate `run_pm_task_with_persona` now relies on.
#[test]
fn filter_persona_tool_names_respects_allow_globs() {
    let all_names = vec![
        "delegate_to_agent".to_string(),
        "add_project".to_string(),
        "list_projects".to_string(),
        "git_log".to_string(),
        "git_status".to_string(),
        "mcp_list".to_string(),
        "run_bash".to_string(),
    ];
    let patterns = vec!["git_*".to_string(), "mcp_list".to_string()];
    let allowed_by_tier: std::collections::HashSet<String> = all_names.iter().cloned().collect();

    let kept = filter_persona_tool_names(
        all_names,
        &patterns,
        &allowed_by_tier,
        &std::collections::HashMap::new(),
        &[],
    );

    assert_eq!(
        kept,
        vec![
            "git_log".to_string(),
            "git_status".to_string(),
            "mcp_list".to_string(),
        ]
    );
}

/// #3285's core security property: `run_pm_task_with_persona` now
/// REGISTERS the delegation/CTRL tools (`delegate_to_agent`,
/// `add_project`, …) into every persona's registry for tool parity with
/// the session path — but registering a tool must not be conflated with
/// granting it. A persona whose `[tools].allow` list (e.g. today's
/// `personal-assistant.toml`) never names those tools must not have them
/// surfaced, even though they're now present in the underlying registry.
#[test]
fn filter_persona_tool_names_new_delegation_tools_require_explicit_allow() {
    let all_names = vec![
        "delegate_to_agent".to_string(),
        "add_project".to_string(),
        "list_projects".to_string(),
        "remove_project".to_string(),
        "stop_task".to_string(),
        "set_active_project".to_string(),
        "move_file".to_string(),
        "create_dir".to_string(),
        "search_code".to_string(),
        "run_bash".to_string(),
        "git_log".to_string(),
    ];
    // A narrow persona allowlist that never mentions the newly-wired
    // delegation/CTRL tools.
    let patterns = vec!["git_log".to_string()];
    let allowed_by_tier: std::collections::HashSet<String> = all_names.iter().cloned().collect();

    let kept = filter_persona_tool_names(
        all_names,
        &patterns,
        &allowed_by_tier,
        &std::collections::HashMap::new(),
        &[],
    );

    assert_eq!(kept, vec!["git_log".to_string()]);
    for forbidden in [
        "delegate_to_agent",
        "add_project",
        "list_projects",
        "remove_project",
        "stop_task",
        "set_active_project",
        "move_file",
        "create_dir",
        "search_code",
        "run_bash",
    ] {
        assert!(
            !kept.iter().any(|k| k == forbidden),
            "{forbidden} must not surface without an explicit allow entry"
        );
    }
}

/// The flip side of the previous test: an "assistant" persona configured
/// WITH delegation rights (an explicit `delegate_to_agent` / `add_project`
/// entry in `[tools].allow`) DOES get those tools surfaced — proving the
/// #3285 parity wiring actually takes effect once a persona opts in.
#[test]
fn filter_persona_tool_names_delegation_tools_surface_when_allowed() {
    let all_names = vec![
        "delegate_to_agent".to_string(),
        "add_project".to_string(),
        "git_log".to_string(),
    ];
    let patterns = vec!["delegate_to_agent".to_string(), "add_project".to_string()];
    let allowed_by_tier: std::collections::HashSet<String> = all_names.iter().cloned().collect();

    let kept = filter_persona_tool_names(
        all_names,
        &patterns,
        &allowed_by_tier,
        &std::collections::HashMap::new(),
        &[],
    );

    assert_eq!(
        kept,
        vec!["delegate_to_agent".to_string(), "add_project".to_string()]
    );
}

/// #3052: izzie's three new platform-hosted tools (`get_weather`,
/// `get_train_schedule`, `get_train_alerts`) — registered unconditionally
/// into the persona registry — are admitted through the filter by izzie's
/// exact-name `[tools].allow` entries. They carry no OpenRPC scope and no
/// restricted tier, so they pass the tier + scope gates unconditionally, the
/// same way `system_status` does. Regression guard that adding the tools to
/// the registry AND the allowlist actually surfaces them.
#[test]
fn filter_persona_tool_names_admits_izzie_weather_and_metro_tools() {
    let all_names = vec![
        "get_weather".to_string(),
        "get_train_schedule".to_string(),
        "get_train_alerts".to_string(),
        // a co-registered tool izzie does NOT allow — must stay hidden
        "run_bash".to_string(),
    ];
    // The exact entries added to `izzie/agent.toml`'s `[tools].allow`.
    let patterns = vec![
        "get_weather".to_string(),
        "get_train_schedule".to_string(),
        "get_train_alerts".to_string(),
        "web_search".to_string(),
    ];
    let allowed_by_tier: std::collections::HashSet<String> = all_names.iter().cloned().collect();

    let kept = filter_persona_tool_names(
        all_names,
        &patterns,
        &allowed_by_tier,
        &std::collections::HashMap::new(),
        &[],
    );

    assert_eq!(
        kept,
        vec![
            "get_weather".to_string(),
            "get_train_schedule".to_string(),
            "get_train_alerts".to_string(),
        ],
        "izzie's three tools surface; run_bash (unallowed) does not"
    );
}

/// RBAC tier filtering is an independent second gate — even a
/// `[tools].allow` glob of `"*"` must not surface a name the tier filter
/// excludes. Mirrors how `allowed_by_tier` (derived from
/// `registry.filter_tools_for_user`) combines with the glob allowlist in
/// production.
#[test]
fn filter_persona_tool_names_respects_rbac_tier() {
    let all_names = vec!["delegate_to_agent".to_string(), "git_log".to_string()];
    let patterns = vec!["*".to_string()];
    let allowed_by_tier: std::collections::HashSet<String> =
        ["git_log".to_string()].into_iter().collect();

    let kept = filter_persona_tool_names(
        all_names,
        &patterns,
        &allowed_by_tier,
        &std::collections::HashMap::new(),
        &[],
    );

    assert_eq!(kept, vec!["git_log".to_string()]);
}

/// #3208 core security property, straight from the issue's own example:
/// a broad allow glob (`"*"`) must NOT bypass an agent's narrower
/// declared scope. A persona whose `[tools].allow` matches everything
/// but whose `[tools].scopes` only covers `google.gmail.read` must have
/// a `google.gmail.send`-scoped tool denied even though the name-based
/// allow gate alone would keep it.
///
/// Pre-fix, `filter_persona_tool_names` had no scope parameters at all
/// and both tools would survive — this test fails against that code
/// (either it doesn't compile against the 3-arg signature, or, if the
/// scope condition is dropped while keeping the signature, `kept`
/// contains the `_send` tool it must not).
#[test]
fn filter_persona_tool_names_respects_declared_scopes() {
    let all_names = vec![
        "gworkspace_read_email".to_string(),
        "gworkspace_send_email".to_string(),
    ];
    let patterns = vec!["*".to_string()];
    let allowed_by_tier: std::collections::HashSet<String> = all_names.iter().cloned().collect();
    let tool_scopes: std::collections::HashMap<String, String> = [
        (
            "gworkspace_read_email".to_string(),
            "google.gmail.read".to_string(),
        ),
        (
            "gworkspace_send_email".to_string(),
            "google.gmail.send".to_string(),
        ),
    ]
    .into_iter()
    .collect();
    let agent_scope_patterns = vec![ScopePattern::new("google.gmail.read")];

    let kept = filter_persona_tool_names(
        all_names,
        &patterns,
        &allowed_by_tier,
        &tool_scopes,
        &agent_scope_patterns,
    );

    assert_eq!(kept, vec!["gworkspace_read_email".to_string()]);
    assert!(
        !kept.iter().any(|k| k == "gworkspace_send_email"),
        "send must be denied by the agent's read-only declared scope"
    );
}

/// Fail-closed companion to the previous test: a persona that declares
/// NO `[tools].scopes` at all must be denied every scoped tool, not
/// granted them by default. `agent_can_use` already denies on an empty
/// pattern list; this pins that the persona wiring actually reaches it.
#[test]
fn filter_persona_tool_names_denies_scoped_tool_when_agent_declares_no_scopes() {
    let all_names = vec!["gworkspace_read_email".to_string()];
    let patterns = vec!["*".to_string()];
    let allowed_by_tier: std::collections::HashSet<String> = all_names.iter().cloned().collect();
    let tool_scopes: std::collections::HashMap<String, String> = [(
        "gworkspace_read_email".to_string(),
        "google.gmail.read".to_string(),
    )]
    .into_iter()
    .collect();
    let agent_scope_patterns: Vec<ScopePattern> = Vec::new();

    let kept = filter_persona_tool_names(
        all_names,
        &patterns,
        &allowed_by_tier,
        &tool_scopes,
        &agent_scope_patterns,
    );

    assert!(
        kept.is_empty(),
        "a scoped tool must be denied when the agent declares no scopes at all"
    );
}

/// Unscoped (native/in-process) tools are unaffected by scope
/// enforcement — they aren't part of the OpenRPC-scoped surface, so
/// their absence from `tool_scopes` must not deny them.
#[test]
fn filter_persona_tool_names_unscoped_tool_ignores_scope_gate() {
    let all_names = vec!["git_log".to_string()];
    let patterns = vec!["*".to_string()];
    let allowed_by_tier: std::collections::HashSet<String> = all_names.iter().cloned().collect();

    let kept = filter_persona_tool_names(
        all_names,
        &patterns,
        &allowed_by_tier,
        &std::collections::HashMap::new(), // git_log declares no scope
        &[],                               // agent declares no scopes either
    );

    assert_eq!(kept, vec!["git_log".to_string()]);
}

/// #3208 regression guard on the OTHER footgun this fix closes: a
/// persona in the `[tools].allow`-configured branch whose surviving
/// tool list is empty (RBAC tier or scope filtering denied everything)
/// must still deny dispatch of every OTHER registered tool. Pre-fix,
/// `run_pm_task_with_persona` collapsed an empty `persona_tool_names` to
/// `allowed_tools = None`, and `ToolRegistry::dispatch_gated` treats
/// `None` as "no restriction" — so a fully-restricted persona would have
/// silently regained unrestricted access to the whole registered
/// surface (`delegate_to_agent`, `run_bash`, ...). This test exercises
/// the exact production chokepoint (`dispatch_gated`) with the exact
/// value `persona_allowed_tools` produces, and fails if
/// `persona_allowed_tools` is reverted to collapse-to-`None`.
#[tokio::test]
async fn persona_allowed_tools_denies_dispatch_when_empty() {
    use crate::tools::ToolRegistry;
    use crate::tools::traits::{ToolExecutor, ToolResult};
    use async_trait::async_trait;

    struct DangerousTool;
    #[async_trait]
    impl ToolExecutor for DangerousTool {
        fn name(&self) -> &str {
            "run_bash"
        }
        fn schema(&self) -> serde_json::Value {
            serde_json::json!({"type":"function","function":{"name":"run_bash","description":"shell","parameters":{"type":"object","properties":{}}}})
        }
        async fn execute(&self, _args: serde_json::Value) -> ToolResult {
            ToolResult::ok("ran a command")
        }
    }

    let mut registry = ToolRegistry::new();
    registry.register(std::sync::Arc::new(DangerousTool));

    // Mirrors a persona whose allow/tier/scope filtering left it with
    // zero permitted tools, even though `run_bash` is still registered.
    let allowed_tools = persona_allowed_tools(Vec::new());
    let result = registry
        .dispatch_gated("run_bash", serde_json::json!({}), allowed_tools.as_deref())
        .await;
    assert!(
        result.is_error(),
        "a persona with zero permitted tools must not be able to dispatch \
             any registered tool: {}",
        result.content()
    );
}

/// The flip side: a non-empty permitted list still allows dispatch of a
/// permitted tool through the same production path.
#[tokio::test]
async fn persona_allowed_tools_permits_dispatch_when_non_empty() {
    use crate::tools::ToolRegistry;
    use crate::tools::traits::{ToolExecutor, ToolResult};
    use async_trait::async_trait;

    struct SearchTool;
    #[async_trait]
    impl ToolExecutor for SearchTool {
        fn name(&self) -> &str {
            "search_code"
        }
        fn schema(&self) -> serde_json::Value {
            serde_json::json!({"type":"function","function":{"name":"search_code","description":"search","parameters":{"type":"object","properties":{}}}})
        }
        async fn execute(&self, _args: serde_json::Value) -> ToolResult {
            ToolResult::ok("search results")
        }
    }

    let mut registry = ToolRegistry::new();
    registry.register(std::sync::Arc::new(SearchTool));

    let allowed_tools = persona_allowed_tools(vec!["search_code".to_string()]);
    let result = registry
        .dispatch_gated(
            "search_code",
            serde_json::json!({}),
            allowed_tools.as_deref(),
        )
        .await;
    assert!(!result.is_error(), "permitted tool must still dispatch");
    assert_eq!(result.content(), "search results");
}

/// Builds a minimal parsed `LlmParams`, optionally declaring
/// `persona_max_turns`, for `persona_max_turns()` unit tests below.
fn llm_params_with_persona_max_turns(persona_max_turns: Option<u32>) -> crate::agents::LlmParams {
    let override_line = persona_max_turns
        .map(|v| format!("persona_max_turns = {v}"))
        .unwrap_or_default();
    let toml_str = format!(
        r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024
{override_line}

[system_prompt]
content = "base"
"#
    );
    let cfg: AgentConfig = toml::from_str(&toml_str).expect("parses");
    cfg.llm
}

/// The no-tools branch is unaffected by this fix — pins current behavior.
#[test]
fn persona_max_turns_no_tools_is_two() {
    let llm = llm_params_with_persona_max_turns(None);
    assert_eq!(persona_max_turns(false, &llm), 2);
}

/// Demo-day fix (2026-07-24): a tool-using persona with no explicit
/// `persona_max_turns` now gets 8 turns, not the prior hardcoded 4 that
/// caused `chat_with_tools exceeded max_turns` failures on multi-tool turns.
#[test]
fn persona_max_turns_with_tools_defaults_to_eight() {
    let llm = llm_params_with_persona_max_turns(None);
    assert_eq!(persona_max_turns(true, &llm), 8);
}

/// An operator can raise (or lower) the ceiling further via
/// `[llm].persona_max_turns` without a rebuild.
#[test]
fn persona_max_turns_with_tools_honors_config_override() {
    let llm = llm_params_with_persona_max_turns(Some(12));
    assert_eq!(persona_max_turns(true, &llm), 12);
}
