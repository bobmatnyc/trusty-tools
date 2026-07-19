//! Persona-agent chat turn (#254).
//!
//! Why: The REPL `/agent` command switches the active ctrl conversation to a
//! non-coding persona (e.g. `personal-assistant`). These personas have their
//! own system prompt, model, and tools-gated registry — distinct enough from
//! the history-delegation path to live in their own file.
//! What: `run_pm_task_with_persona`.
//! Test: Manual via tmux — `/agent personal-assistant` then "who are you?".

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

use crate::agents::AgentConfig;
use crate::llm;
use crate::subprocess::SubprocessAgentRunner;
use crate::tools::{AgentRunner, ToolRegistry, delegate::DelegateToAgentTool};

use super::super::super::claude_cli::run_pm_task_via_claude_cli;
use super::super::super::config::{
    SessionOverrides, apply_credential_routing, build_user_context_prefix,
    resolve_overridden_credentials,
};
use super::super::super::handlers::{
    AddProjectTool, CreateDirTool, ListProjectsTool, MoveFileTool, RemoveProjectTool,
    SetActiveProjectTool, StopTaskTool, register_ticketing_tools,
};
use super::super::super::state::ConversationTurn;
use super::super::helpers::match_any_glob;

/// Narrow a persona's full candidate tool-name list down to what it may
/// actually reach on this turn (#3285).
///
/// Why: `run_pm_task_with_persona` now registers the FULL delegation/CTRL/
/// filesystem/search/shell/MCP tool surface into every persona's registry —
/// tool parity with the session path. Without a second, independent gate a
/// persona would suddenly gain every tool the PM has, regardless of what its
/// TOML declares. This function is that gate: it is the single place that
/// decides which of the registered tools are actually advertised to the LLM,
/// so it is pulled out as a pure function precisely so the allow/scope
/// security property can be pinned by fast unit tests instead of only being
/// exercised end-to-end.
/// What: Keeps a name only when it matches at least one glob in `patterns`
/// (a persona's `[tools].allow` list — see `match_any_glob`) AND is present
/// in `allowed_by_tier` (the RBAC-tier filter derived from
/// `registry.filter_tools_for_user`). Order-preserving; both gates must pass
/// independently, mirroring how `registry`+`allowed_tools` combine in
/// `chat_with_tools_gated`.
/// Test: `filter_persona_tool_names_respects_allow_globs`,
/// `filter_persona_tool_names_new_delegation_tools_require_explicit_allow`,
/// `filter_persona_tool_names_delegation_tools_surface_when_allowed`,
/// `filter_persona_tool_names_respects_rbac_tier`.
fn filter_persona_tool_names(
    all_names: Vec<String>,
    patterns: &[String],
    allowed_by_tier: &std::collections::HashSet<String>,
) -> Vec<String> {
    all_names
        .into_iter()
        .filter(|name| match_any_glob(name, patterns) && allowed_by_tier.contains(name))
        .collect()
}

/// Run a single conversation turn against a persona agent (#254).
///
/// Why: The REPL `/agent` command lets the user switch the active ctrl
/// conversation to a non-coding persona (e.g. `personal-assistant` /
/// `cto-assistant`). #3285 (Bob decision, 2026-07-19): named personas must
/// be able to call tools directly, so this path now reaches TOOL PARITY
/// with the session path (`run_pm_task_with_history`) — the same
/// delegation (`delegate_to_agent`), CTRL project-management
/// (`add_project`, `list_projects`, `remove_project`, `stop_task`,
/// `set_active_project`), filesystem (`move_file`, `create_dir`), search
/// (`search_code`), shell (`run_bash`), ticketing, `tm`, and live-MCP tools
/// are all REGISTERED into the persona's registry below. Registering a tool
/// is not the same as granting it, though: what actually reaches the LLM is
/// still gated per persona by `filter_persona_tool_names`, which keeps only
/// names matching the persona's `[tools].allow` glob list AND passing the
/// RBAC-tier filter (`registry.filter_tools_for_user`) — a persona can only
/// reach what it explicitly declares. A persona with no `[tools].allow`
/// section at all still gets zero tools (see the `else` arm below),
/// preserving the original tools-OFF-by-default posture for personas that
/// don't opt in. Full `[tools].scopes` (OpenRPC scope-pattern) enforcement
/// is tracked separately by #3208 and is not implemented here; this change
/// does not widen a persona's reach beyond its declared `allow` list.
/// What: Loads `<project>/.trusty-agents/agents/<persona_name>.toml`, builds the
/// same date/time-injected system prompt the default ctrl path uses, arms a
/// `ToolRegistry` filtered per the persona's declared `allow` globs, then
/// makes a `chat_with_tools_gated` call carrying the prior conversation
/// history. Returns the assistant text.
/// Test: `filter_persona_tool_names_respects_allow_globs`,
/// `filter_persona_tool_names_new_delegation_tools_require_explicit_allow`,
/// `filter_persona_tool_names_delegation_tools_surface_when_allowed`,
/// `filter_persona_tool_names_respects_rbac_tier` pin the allow/scope
/// gating at the unit level. Manual via tmux — `/agent personal-assistant`
/// then "who are you?" → identifies as Izzie, knows Masa.
pub async fn run_pm_task_with_persona(
    project_path: &Path,
    persona_name: &str,
    user_input: &str,
    history: &[ConversationTurn],
    session_id: Option<String>,
    overrides: SessionOverrides,
) -> Result<String> {
    use async_openai::types::{
        ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestMessage,
        ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs,
    };

    let sid = session_id.unwrap_or_default();

    // #3223/#3224 (Trusty Assistant agent roster, epic #3052): resolve
    // through the canonical `AgentConfig::by_name_async` loader instead of
    // the hand-rolled project/$HOME `.toml`-only lookup this function used
    // to do inline. `by_name_async` walks the SAME tier order the old code
    // did (project `.trusty-agents/agents/` then `$HOME/.trusty-agents/agents`,
    // #3061) but ALSO resolves flat `.md` personalization overlays (with
    // their `extends:` chain, #3055) and directory-package agents — neither
    // of which the old inline lookup understood. Practically: a roster
    // entry backed by a user's `~/.trusty-agents/agents/<slug>.md` overlay
    // (written by the personalization editor) now dispatches correctly
    // through this persona-chat path, not just the subprocess `--direct`
    // path that already used `by_name_async` internally.
    let mut persona_cfg = AgentConfig::by_name_async(persona_name)
        .await
        .with_context(|| format!("persona agent '{persona_name}' not found"))?;

    if let Some(ref m) = overrides.model {
        tracing::debug!(persona = %persona_name, model = %m, "applying /model override");
        persona_cfg.agent.model = m.clone();
    }

    let _ = sid;
    let creds = resolve_overridden_credentials(&mut persona_cfg, overrides.provider.as_deref())?;
    let claude_cli_short_circuit = apply_credential_routing(&mut persona_cfg, &creds);
    tracing::info!(
        persona = %persona_name,
        agent = %persona_cfg.agent.name,
        runner = ?persona_cfg.agent.runner,
        model = %persona_cfg.agent.model,
        creds = creds.label(),
        claude_cli_short_circuit,
        use_anthropic_direct = persona_cfg.llm.use_anthropic_direct,
        "run_pm_task_with_persona: credentials resolved"
    );
    if claude_cli_short_circuit {
        return run_pm_task_via_claude_cli(project_path, &persona_cfg, user_input, history, "")
            .await;
    }
    let persona_llm_t0 = std::time::Instant::now();

    let client = llm::create_client()?;

    let (persona_registry, persona_tool_names): (ToolRegistry, Vec<String>) =
        if let Some(patterns) = persona_cfg.tools.allow.clone() {
            let mut registry = ToolRegistry::new();
            for tool in crate::tools::mcp_tools::mcp_tool_executors() {
                registry.register(tool);
            }
            for tool in crate::tools::mcp_service_tools::mcp_service_tool_executors().await {
                registry.register(tool);
            }
            {
                let global_config = crate::mcp::config::GlobalConfig::load().await;
                match crate::tools::registry::ToolRegistryBuilder::from_config(&global_config)
                    .build()
                    .await
                {
                    Ok(execs) => {
                        for tool in execs {
                            registry.register(tool);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("tool registry init failed: {e}");
                    }
                }
            }
            if let Ok(repo) = crate::git::GitRepo::open(
                &std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            ) {
                for tool in crate::tools::git_tools::git_tools(repo.root.clone()) {
                    registry.register(tool);
                }
            }
            register_ticketing_tools(&mut registry).await;

            // #3285: tool parity with the session path
            // (`run_pm_task_with_history`) — delegation, CTRL
            // project-management, filesystem, search, and shell tools are
            // now registered here too. Whether a persona can actually reach
            // any of them is still decided below by `filter_persona_tool_names`
            // against `patterns` (the persona's `[tools].allow` globs), so
            // registering them here does not by itself grant access.
            let config_dir = project_path.join(".trusty-agents").join("agents");
            let runner: Arc<dyn AgentRunner> =
                Arc::new(SubprocessAgentRunner::new().with_config_dir(Some(config_dir.clone())));
            registry.register(Arc::new(
                DelegateToAgentTool::new(runner).with_config_dir(config_dir.clone()),
            ));
            registry.register(Arc::new(AddProjectTool));
            registry.register(Arc::new(ListProjectsTool));
            registry.register(Arc::new(RemoveProjectTool));
            registry.register(Arc::new(StopTaskTool {
                snapshot: Vec::new(),
                pending_stop: Arc::new(Mutex::new(None)),
            }));
            registry.register(Arc::new(SetActiveProjectTool {
                active_project: Arc::new(Mutex::new(None)),
            }));
            registry.register(Arc::new(MoveFileTool));
            registry.register(Arc::new(CreateDirTool));
            {
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let search_tool = crate::tools::native_search::SearchCodeTool::new_auto(&cwd).await;
                registry.register(Arc::new(search_tool));
            }
            {
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                registry.register(Arc::new(crate::tools::run_bash::RunBashTool::new(cwd)));
            }
            {
                let state_dir = project_path.join(".trusty-agents").join("state");
                crate::tools::tm_tools::register_tm_tools_for_state_dir(&mut registry, &state_dir);
            }

            registry.register(Arc::new(
                crate::tools::web_search::BraveSearchTool::from_env(),
            ));

            for plugin in crate::tools::agent_plugin::plugins_for_persona(persona_name) {
                for tool in &plugin.tools {
                    registry.register(std::sync::Arc::clone(tool));
                }
            }

            // #3238: live-discover MCP servers (`[[mcp.services]] discover =
            // true` and `.mcp.json`) and register whatever tools they
            // actually advertise, on top of everything registered above.
            // `existing_names` seeds the dedup set so a live-discovered tool
            // never shadows a tool already registered from another source.
            {
                let existing_names: std::collections::HashSet<String> = registry
                    .schemas()
                    .iter()
                    .filter_map(|s| {
                        s.get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                            .map(String::from)
                    })
                    .collect();
                for tool in
                    crate::tools::mcp_live::live_mcp_tool_executors(project_path, &existing_names)
                        .await
                {
                    registry.register(tool);
                }
            }

            let all_names: Vec<String> = registry
                .schemas()
                .into_iter()
                .filter_map(|s| {
                    s.get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .map(String::from)
                })
                .collect();
            let rbac_user = overrides.user.clone().unwrap_or_default();
            let allowed_by_tier: std::collections::HashSet<String> = registry
                .filter_tools_for_user(&rbac_user)
                .into_iter()
                .map(|t| t.schema())
                .filter_map(|s| {
                    s.get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .map(String::from)
                })
                .collect();
            let kept = filter_persona_tool_names(all_names, &patterns, &allowed_by_tier);
            tracing::info!(
                persona = %persona_name,
                tools = ?kept,
                rbac_user = %rbac_user.id,
                rbac_tier = ?rbac_user.tier,
                "persona tool registry built"
            );
            (registry, kept)
        } else {
            (ToolRegistry::new(), Vec::new())
        };

    let system_prompt: String = {
        let base = build_user_context_prefix(&persona_cfg.system_prompt.content);
        let runner_label = match persona_cfg.agent.runner {
            crate::agents::RunnerKind::Subprocess => "subprocess",
            crate::agents::RunnerKind::Inline => "inline",
            crate::agents::RunnerKind::ClaudeCode => "claude-code",
            crate::agents::RunnerKind::InProcess => "in-process",
        };
        let base = crate::agents::prompt_builder::SystemPromptBuilder::new(base)
            .with_agent_context(persona_cfg.agent.model.as_str(), runner_label)
            .build();
        if !persona_tool_names.is_empty() {
            format!(
                "{}\n\n## Available tools\nYou have access to the following tools: {}.\nUse them when the user asks questions that require live data.",
                base,
                persona_tool_names.join(", ")
            )
        } else {
            base
        }
    };

    let mut initial_messages: Vec<ChatCompletionRequestMessage> = Vec::new();
    initial_messages.push(
        ChatCompletionRequestSystemMessageArgs::default()
            .content(system_prompt)
            .build()
            .context("failed to build persona system message")?
            .into(),
    );
    for turn in history {
        initial_messages.push(
            ChatCompletionRequestUserMessageArgs::default()
                .content(turn.user.clone())
                .build()
                .context("failed to build persona history user message")?
                .into(),
        );
        initial_messages.push(
            ChatCompletionRequestAssistantMessageArgs::default()
                .content(turn.assistant.clone())
                .build()
                .context("failed to build persona history assistant message")?
                .into(),
        );
    }
    initial_messages.push(
        ChatCompletionRequestUserMessageArgs::default()
            .content(user_input)
            .build()
            .context("failed to build persona current user message")?
            .into(),
    );

    let adapter = llm::adapter::adapter_for_model(&persona_cfg.agent.model);
    let allowed_tools = if persona_tool_names.is_empty() {
        None
    } else {
        Some(persona_tool_names.clone())
    };
    let max_turns = if persona_tool_names.is_empty() { 2 } else { 4 };
    let (content, _usage) = llm::chat_with_tools_gated(
        &client,
        &persona_cfg.agent.model,
        &*adapter,
        initial_messages,
        Arc::new(persona_registry),
        allowed_tools,
        persona_cfg.llm.temperature,
        persona_cfg.llm.max_tokens,
        max_turns,
        false,
        None,
        false,
        persona_cfg.llm.use_anthropic_direct,
        &persona_cfg.llm.stop_sequences,
    )
    .await
    .context("persona LLM call failed")?;
    tracing::info!(
        persona = %persona_name,
        llm_ms = persona_llm_t0.elapsed().as_millis() as u64,
        response_chars = content.len(),
        "run_pm_task_with_persona: LLM call complete"
    );

    Ok(content)
}

#[cfg(test)]
mod tests {
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
        let allowed_by_tier: std::collections::HashSet<String> =
            all_names.iter().cloned().collect();

        let kept = filter_persona_tool_names(all_names, &patterns, &allowed_by_tier);

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
        let allowed_by_tier: std::collections::HashSet<String> =
            all_names.iter().cloned().collect();

        let kept = filter_persona_tool_names(all_names, &patterns, &allowed_by_tier);

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
        let allowed_by_tier: std::collections::HashSet<String> =
            all_names.iter().cloned().collect();

        let kept = filter_persona_tool_names(all_names, &patterns, &allowed_by_tier);

        assert_eq!(
            kept,
            vec!["delegate_to_agent".to_string(), "add_project".to_string()]
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

        let kept = filter_persona_tool_names(all_names, &patterns, &allowed_by_tier);

        assert_eq!(kept, vec!["git_log".to_string()]);
    }
}
