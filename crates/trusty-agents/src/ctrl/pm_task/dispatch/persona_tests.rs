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
// #4171: the pure gating helpers moved to `persona_gate` (see that
// module's doc comment for why); imported directly so these tests keep
// exercising the same functions, unchanged.
use super::super::persona_gate::filter_persona_tool_names;
// `persona.rs` no longer imports `Scope` (its only user moved to
// `persona_gate`), but these tests still construct one directly.
use crate::tools::registry::scope::Scope;
// #4201: the delegation-gate tests below EXECUTE the real tool this path
// registers, so they need the trait that carries `execute`.
use crate::tools::traits::ToolExecutor;

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

/// #3938 regression pin: the bundled `cto-assistant` DIRECTORY PACKAGE must
/// carry enough Google scope to survive the dispatch-time scope
/// intersection for its Gmail/Tasks surface.
///
/// Why: the package (`.trusty-agents/agents/cto-assistant/agent.toml`) WINS
/// over the flat `cto-assistant.toml` in `by_name_unresolved_src_in`, and
/// when it was converted from the flat form its `scopes = [...]` line was
/// dropped. With none declared it inherited only the base assistant's
/// `google.read` by union (`extends::merge_extends`) — a pattern that
/// matches NO scope any gworkspace tool advertises (every dotted scope is
/// `google.<family>.<access>`; see `registry::google_scope`), so
/// `agent_can_use` denied the whole Gmail/Tasks surface even though every
/// tool was allowlisted by name. Asserting on the checked-in package rather
/// than a synthetic fixture is the point: this pins the SHIPPED config, so
/// re-dropping the line (or converting another agent the same way) fails
/// here instead of at a user's dispatch.
/// What: resolves the real package through the real loader (`by_name_in`,
/// including its `extends = "assistant"` union), derives each tool's dotted
/// scope from the real `trusty-gworkspace` `rpc.discover` document through
/// the real `parse_manifest` -> `x-google-scopes` mapping, and runs the real
/// `filter_persona_tool_names` gate. No daemon, no network, no LLM.
#[test]
fn cto_assistant_package_gmail_and_tasks_tools_survive_scope_intersection() {
    // The scoped gworkspace tools this persona is built around: Gmail triage
    // (#473, inherited from the base's Gmail surface) and Google Tasks
    // (#472/#485, this package's own delta). `create_draft` / `create_task`
    // are deliberately absent — they are legacy names gworkspace no longer
    // exposes, so they carry no scope and are filtered by registration, not
    // by scope (see `skills::manifest::builtin::gworkspace`).
    const GMAIL_AND_TASKS: &[&str] = &[
        "search_gmail_messages",
        "get_gmail_message_content",
        "modify_gmail_messages",
        "list_tasks",
        "complete_task",
    ];

    let agents_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".trusty-agents")
        .join("agents");
    let cfg = AgentConfig::by_name_in(&[agents_dir], "cto-assistant")
        .expect("bundled cto-assistant directory package resolves");

    let manifest = crate::tools::registry::discovery::parse_manifest(
        "gworkspace",
        &trusty_gworkspace::openrpc::discover_response(),
    )
    .expect("gworkspace rpc.discover document parses");
    let tool_scopes: std::collections::HashMap<String, String> = manifest
        .tools
        .iter()
        .map(|t| (t.name.clone(), t.scope.clone()))
        .collect();

    // Guard against a vacuous pass: every name below must still BE a scoped
    // gworkspace tool, otherwise the scope gate would wave it through on the
    // `None` branch and prove nothing.
    for name in GMAIL_AND_TASKS {
        assert!(
            tool_scopes.contains_key(*name),
            "{name} no longer advertises a gworkspace scope — refresh this list"
        );
    }

    let allow = cfg
        .tools
        .allow
        .clone()
        .expect("cto-assistant declares [tools].allow");
    let scope_patterns: Vec<ScopePattern> = cfg
        .tools
        .scopes
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(ScopePattern::new)
        .collect();

    let all_names: Vec<String> = GMAIL_AND_TASKS.iter().map(|s| (*s).to_string()).collect();
    let allowed_by_tier: std::collections::HashSet<String> = all_names.iter().cloned().collect();

    let survivors = filter_persona_tool_names(
        all_names.clone(),
        &allow,
        &allowed_by_tier,
        &tool_scopes,
        &scope_patterns,
    );

    assert_eq!(
        survivors, all_names,
        "cto-assistant's Gmail/Tasks tools were scope-denied at dispatch; \
         resolved [tools].scopes = {:?}",
        cfg.tools.scopes
    );
}

/// #3987 (option B) pinned against the SHIPPED base template: the bundled
/// `assistant` declares NO dead scope pattern.
///
/// Why assert on the checked-in config rather than a literal: the diagnostic's
/// value is that it fires on the real defect, and the real defect was in the
/// file we ship. Resolving through the real loader also proves the check sees
/// the POST-`extends` scope set, which is the set the dispatch gate uses.
///
/// **This assertion was INVERTED by option B**, and the inversion is the
/// point. Option C (the preceding PR) landed this test asserting the base's
/// `google.read` WAS dead — that is why option C shipped as a `warn!` rather
/// than a hard load error, since failing closed would have broken every
/// `extends = "assistant"` load. Option B removed the dead pattern, so the
/// base is now clean and the sequencing constraint is discharged: escalating
/// `persona.rs`'s warning to a hard error is unblocked, and this test is what
/// will keep it unblocked.
#[test]
fn base_assistant_declares_no_dead_scope_patterns() {
    use crate::tools::registry::dead_scope;

    let agents_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".trusty-agents")
        .join("agents");
    let cfg = AgentConfig::by_name_in(&[agents_dir], "assistant")
        .expect("bundled assistant template resolves");
    let scopes = cfg
        .tools
        .scopes
        .clone()
        .expect("the base assistant declares [tools].scopes");
    let patterns: Vec<ScopePattern> = scopes.iter().cloned().map(ScopePattern::new).collect();

    // No live registry: the static Google vocabulary alone settles this under
    // ANY registry state.
    let dead = dead_scope::dead_scope_patterns(
        &patterns,
        &dead_scope::reachable_scopes(std::iter::empty::<String>()),
    );
    assert!(
        dead.is_empty(),
        "the base assistant carries dead scope grants: {dead:?} (declared: {scopes:?})"
    );
    // Guard against a vacuous pass in the other direction: the dead pattern
    // this issue is about must be GONE, not merely un-flagged because the
    // whole scopes line was deleted.
    assert!(
        !scopes.iter().any(|s| s == "google.read"),
        "the dead `google.read` pattern is still declared: {scopes:?}"
    );
    assert!(
        scopes.iter().any(|s| s == "google.gmail.*"),
        "the base must still grant the Gmail surface its allowlist names: {scopes:?}"
    );
}

/// #3987 (option B): every scoped gworkspace tool the base assistant
/// allowlists must survive the dispatch-time scope intersection — and every
/// Google family the base grants must be earned by at least one allowlisted
/// tool.
///
/// Why both directions: the first half is the defect (#3987 — all ~60
/// allowlisted gworkspace tools were denied). The second half is the guard on
/// the FIX: option B is a posture change on the shared base, so a family
/// added here without an allowlisted tool behind it would be exactly the
/// speculative widening the issue warned against. Asserting it in code is
/// what keeps the audit table in `assistant/agent.toml` honest as the
/// allowlist evolves.
/// What: resolves the SHIPPED base through the real loader, derives each
/// tool's dotted scope from the real `trusty-gworkspace` `rpc.discover`
/// document through the real `parse_manifest`, and runs the real
/// `filter_persona_tool_names` gate. No daemon, no network, no LLM — same
/// harness shape as `cto_assistant_package_gmail_and_tasks_tools_survive_scope_intersection`
/// (#3985).
#[test]
fn base_assistant_scope_grants_cover_every_allowlisted_gworkspace_tool() {
    let agents_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".trusty-agents")
        .join("agents");
    let cfg = AgentConfig::by_name_in(&[agents_dir], "assistant")
        .expect("bundled assistant template resolves");

    let manifest = crate::tools::registry::discovery::parse_manifest(
        "gworkspace",
        &trusty_gworkspace::openrpc::discover_response(),
    )
    .expect("gworkspace rpc.discover document parses");
    // A tool whose scope could not be derived gets `""` from discovery and is
    // dropped by the ENDPOINT scope filter long before an agent sees it
    // (`format_email_content`, a pure local transform, is the only such tool
    // the base allowlists). Excluding those here keeps the assertion about
    // agent scope grants rather than about discovery.
    let tool_scopes: std::collections::HashMap<String, String> = manifest
        .tools
        .iter()
        .filter(|t| !t.scope.is_empty())
        .map(|t| (t.name.clone(), t.scope.clone()))
        .collect();

    let allow = cfg
        .tools
        .allow
        .clone()
        .expect("the base assistant declares [tools].allow");
    let scopes = cfg
        .tools
        .scopes
        .clone()
        .expect("the base assistant declares [tools].scopes");
    let patterns: Vec<ScopePattern> = scopes.iter().cloned().map(ScopePattern::new).collect();

    // Exact allowlist entries that ARE scoped gworkspace tools. Glob entries
    // (`granola_*`) and names gworkspace no longer exposes (`sync_drive`,
    // `convert_document`, `publish_markdown_to_doc`, `render_mermaid_to_doc`,
    // `format_slide`) are filtered by REGISTRATION, not by scope, so they are
    // not this test's subject.
    let scoped: Vec<String> = allow
        .iter()
        .filter(|n| !n.contains('*'))
        .filter(|n| tool_scopes.contains_key(*n))
        .cloned()
        .collect();
    assert!(
        scoped.len() >= 50,
        "expected the base's large gworkspace surface, got {} — has the \
         allowlist or the manifest changed? {scoped:?}",
        scoped.len()
    );

    let allowed_by_tier: std::collections::HashSet<String> = scoped.iter().cloned().collect();
    let survivors = filter_persona_tool_names(
        scoped.clone(),
        &allow,
        &allowed_by_tier,
        &tool_scopes,
        &patterns,
    );
    assert_eq!(
        survivors, scoped,
        "base assistant gworkspace tools were scope-denied at dispatch; \
         resolved [tools].scopes = {scopes:?}"
    );

    // No speculative grants: every declared `google.*` family must be needed.
    for pattern in patterns
        .iter()
        .filter(|p| p.as_str().starts_with("google."))
    {
        assert!(
            scoped
                .iter()
                .any(|name| { pattern.matches(&Scope::new(tool_scopes[name].clone())) }),
            "`{}` is granted but no allowlisted tool needs it — grant only \
             families the allow-list actually names (#3987)",
            pattern.as_str()
        );
    }
}

/// #3987 the negative half: a live family/wildcard pattern is never flagged,
/// and option B introduces no double-grant weirdness in the overlays.
/// `izzie` ships blanket `google.*`; `cto-assistant` ships the narrow
/// `google.gmail.*` / `google.tasks.*` pair (#3985), which the base now also
/// grants — `extends::union_opt_vec` dedups exact string matches, so the
/// duplicates collapse rather than accumulating.
#[test]
fn shipped_agents_with_live_google_patterns_are_not_flagged() {
    use crate::tools::registry::dead_scope;

    let agents_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".trusty-agents")
        .join("agents");
    let reachable = dead_scope::reachable_scopes(std::iter::empty::<String>());
    for name in ["izzie", "cto-assistant"] {
        let cfg = AgentConfig::by_name_in(std::slice::from_ref(&agents_dir), name)
            .unwrap_or_else(|e| panic!("bundled {name} resolves: {e}"));
        let scopes = cfg.tools.scopes.clone().unwrap_or_default();
        let patterns: Vec<ScopePattern> = scopes.iter().cloned().map(ScopePattern::new).collect();
        let dead = dead_scope::dead_scope_patterns(&patterns, &reachable);
        // Post-option-B these overlays inherit only live patterns, so the
        // resolved set must be completely clean — no `google.read` left to
        // tolerate.
        assert!(
            dead.is_empty(),
            "{name}: dead scope grants after option B: {dead:?} (declared: {scopes:?})"
        );
        let mut deduped = scopes.clone();
        deduped.sort();
        deduped.dedup();
        assert_eq!(
            deduped.len(),
            scopes.len(),
            "{name}: the extends union produced duplicate scope patterns: {scopes:?}"
        );
    }
}

/// #4171 (epic #4167): an L1 persona that DECLARES the session-state tools
/// still gets none of them out of the persona-chat gate.
///
/// Why: `session_list`, `session_status`, `project_list` and
/// `console_metrics` are registered into every persona's registry by the
/// trusty-mpm MCP service, and `system_status` natively; before #4171 the L1
/// black-box posture held them back only by their ABSENCE from each persona's
/// `[tools].allow`. This test pins the enforcement that replaced that
/// convention, at the exact function `run_pm_task_with_persona` calls.
/// What: an allow-list naming every gated tool plus `git_log`, an
/// unrestricted RBAC tier and no scope requirements — so the first three
/// gates admit everything — leaves only `git_log` once the tier gate runs at
/// `AgentTier::L1Standard`.
/// Test: this function IS the test.
#[test]
fn persona_tier_gate_strips_session_state_for_l1() {
    let all_names: Vec<String> = crate::tools::session_state::L0_ONLY_SESSION_STATE_TOOLS
        .iter()
        .map(|s| s.to_string())
        .chain(std::iter::once("git_log".to_string()))
        .collect();
    let patterns = vec!["*".to_string()];
    let allowed_by_tier: std::collections::HashSet<String> = all_names.iter().cloned().collect();

    let kept = super::super::persona_gate::filter_persona_tool_names_for_tier(
        all_names,
        &patterns,
        &allowed_by_tier,
        &std::collections::HashMap::new(),
        &[],
        crate::agents::AgentTier::L1Standard,
    );

    assert_eq!(kept, vec!["git_log".to_string()]);
}

/// #4171 counter-test: the identical declaration on an L0 persona keeps every
/// session-state tool, so the gate is a tier boundary and not a blanket deny.
///
/// Why: a gate that denies everyone is not a grant — issue #4171's acceptance
/// criterion is that L0 CAN reach session state.
/// What: same inputs as `persona_tier_gate_strips_session_state_for_l1` with
/// `AgentTier::L0Orchestration`; nothing is removed.
/// Test: this function IS the test.
#[test]
fn persona_tier_gate_keeps_session_state_for_l0() {
    let all_names: Vec<String> = crate::tools::session_state::L0_ONLY_SESSION_STATE_TOOLS
        .iter()
        .map(|s| s.to_string())
        .chain(std::iter::once("git_log".to_string()))
        .collect();
    let patterns = vec!["*".to_string()];
    let allowed_by_tier: std::collections::HashSet<String> = all_names.iter().cloned().collect();

    let kept = super::super::persona_gate::filter_persona_tool_names_for_tier(
        all_names.clone(),
        &patterns,
        &allowed_by_tier,
        &std::collections::HashMap::new(),
        &[],
        crate::agents::AgentTier::L0Orchestration,
    );

    assert_eq!(kept, all_names);
}

// ---------------------------------------------------------------------------
// #4201: the delegation gate on the PERSONA dispatch path.
//
// These tests deliberately drive `build_persona_delegate_tool` — the single
// function `run_pm_task_with_persona` registers — and then `execute()` the
// resulting tool against a spy runner, rather than re-asserting a posture
// tuple. A test that rebuilt the tool itself would keep passing if the call
// site dropped `.with_allowed_target_roles(...)` again, which is exactly the
// regression #4201 filed. Verified non-vacuous: with that call removed from
// `build_persona_delegate_tool`, the two refusal tests below fail (the spy
// runner is invoked).
// ---------------------------------------------------------------------------

/// Spy runner: records every agent name a delegation actually reached a
/// spawn for. An empty log is the proof a gate rejected BEFORE spawn.
struct SpyRunner {
    spawned: std::sync::Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl AgentRunner for SpyRunner {
    async fn run(
        &self,
        agent_name: &str,
        _task: &str,
    ) -> anyhow::Result<crate::tools::traits::AgentOutput> {
        self.spawned
            .lock()
            .expect("spy runner lock poisoned")
            .push(agent_name.to_string());
        Ok(crate::tools::traits::AgentOutput {
            content: "ok".into(),
            summary: None,
            usage: crate::perf::TokenUsage::default(),
        })
    }
}

/// Seed one resolvable agent TOML with an explicit `role` (and no `tier`, so
/// it fails closed to `L1Standard` exactly like every bundled agent today).
fn seed_agent_with_role(dir: &Path, name: &str, role: &str) {
    std::fs::write(
        dir.join(format!("{name}.toml")),
        format!(
            r#"[agent]
name = "{name}"
role = "{role}"
model = "anthropic/claude-sonnet-4-6"
description = "issue 4201 test fixture"

[llm]
temperature = 0.2
max_tokens = 1024

[system_prompt]
content = "test"
"#
        ),
    )
    .expect("failed to seed agent fixture");
}

/// Run one delegation attempt through the persona path's own tool.
///
/// Returns `(is_error, message, spawn_count)`.
async fn persona_delegate_attempt(
    target_name: &str,
    target_role: &str,
    persona_role: &str,
) -> (bool, String, usize) {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_agent_with_role(dir.path(), target_name, target_role);

    let runner = Arc::new(SpyRunner {
        spawned: std::sync::Mutex::new(Vec::new()),
    });
    let tool = super::super::persona_gate::build_persona_delegate_tool(
        runner.clone(),
        dir.path().to_path_buf(),
        "sess-4201".to_string(),
        persona_role,
        // Every persona shipped today resolves to L1Standard — see the
        // known-gap note: no bundled agent declares `tier = "l0"`, which is
        // precisely why the tier gate cannot cover for the role allowlist.
        crate::agents::AgentTier::L1Standard,
    );

    let result = tool
        .execute(serde_json::json!({
            "agent_name": target_name,
            "task": "#4201 delegation attempt",
        }))
        .await;
    let spawned = runner
        .spawned
        .lock()
        .expect("spy runner lock poisoned")
        .len();
    (result.is_error(), result.content().to_string(), spawned)
}

/// #4201 PRIMARY regression: an assistant-tier persona on the REPL `/agent`
/// path must NOT be able to delegate to `pm` (role `orchestrator`).
///
/// Why: `orchestrator` is not in `ASSISTANT_ALLOWED_DELEGATE_ROLES` at all;
/// `run_subagent` arms the spawned child from ITS OWN role, so a successful
/// spawn here hands an unrestricted orchestrator registry (shell,
/// `write_file`, unrestricted `delegate_to_agent`) to a persona that ingests
/// untrusted content. The #4169 tier gate does not catch this: the fixture
/// declares no `tier`, so the target resolves to `L1Standard` and
/// `tier_blocked` is false — the role allowlist is the ONLY thing standing
/// between this call and the spawn.
/// What: refuses before the runner is touched, and does not leak the target's
/// role taxonomy in the message.
/// Test: this function IS the test.
#[tokio::test]
async fn persona_delegate_tool_refuses_orchestrator_role_target() {
    let (is_error, msg, spawned) =
        persona_delegate_attempt("pm", "orchestrator", "assistant").await;

    assert!(
        is_error,
        "persona path must refuse a role=orchestrator target"
    );
    assert_eq!(spawned, 0, "runner must never be reached: {msg}");
    assert!(
        !msg.to_lowercase().contains("orchestrator"),
        "refusal must not leak the target's role, got: {msg}"
    );
}

/// Sibling of the above for `ctrl` (role `controller`) — the other privileged
/// meta-agent the allowlist excludes.
///
/// Why: `controller` is likewise absent from
/// `ASSISTANT_ALLOWED_DELEGATE_ROLES`; pinning both means a partial fix that
/// special-cased only `pm` cannot pass.
/// What: same refusal, same no-spawn guarantee.
/// Test: this function IS the test.
#[tokio::test]
async fn persona_delegate_tool_refuses_controller_role_target() {
    let (is_error, msg, spawned) =
        persona_delegate_attempt("ctrl", "controller", "assistant").await;

    assert!(
        is_error,
        "persona path must refuse a role=controller target"
    );
    assert_eq!(spawned, 0, "runner must never be reached: {msg}");
}

/// The gate must not become a blanket denial: a worker role IS allowlisted
/// and must still spawn.
///
/// Why: a refusal-only test would pass against a gate that broke every
/// delegation, which would be a worse regression than the hole it closed.
/// What: `engineer` (role `engineer`, in the allowlist) reaches the runner.
/// Test: this function IS the test.
#[tokio::test]
async fn persona_delegate_tool_allows_worker_role_target() {
    let (is_error, msg, spawned) =
        persona_delegate_attempt("engineer", "engineer", "assistant").await;

    assert!(!is_error, "allowlisted worker role must pass: {msg}");
    assert_eq!(spawned, 1, "runner must be reached exactly once");
}

/// The peer-consult lane (Izzie <-> cto-assistant) must survive the gate.
///
/// Why: `ASSISTANT_TIER_ROLE` is in the allowlist precisely so a peer spawn
/// keeps working — safe because the peer is ALSO routed through the
/// restricted assistant-tier registry, never an unrestricted one.
/// What: role `assistant` reaches the runner.
/// Test: this function IS the test.
#[tokio::test]
async fn persona_delegate_tool_allows_peer_assistant_role_target() {
    let (is_error, msg, spawned) =
        persona_delegate_attempt("cto-assistant", "assistant", "assistant").await;

    assert!(!is_error, "peer assistant role must pass: {msg}");
    assert_eq!(spawned, 1, "runner must be reached exactly once");
}

/// A persona whose OWN role is outside the assistant tier model is unchanged
/// by #4201 — no role allowlist, no tier gate.
///
/// Why: `build_persona_delegate_tool`'s `role != "assistant"` branch mirrors
/// `ctrl_delegate_posture`'s, and this pins that the fix did not silently
/// narrow a population it was never meant to touch (the same reason
/// `delegate_without_role_gate_allows_any_resolvable_role` exists for the
/// history path).
/// What: a non-assistant persona delegating to `pm` still spawns.
/// Test: this function IS the test.
#[tokio::test]
async fn persona_delegate_tool_leaves_non_assistant_roles_ungated() {
    let (is_error, msg, spawned) =
        persona_delegate_attempt("pm", "orchestrator", "orchestrator").await;

    assert!(!is_error, "non-assistant persona must stay ungated: {msg}");
    assert_eq!(spawned, 1, "runner must be reached exactly once");
}
