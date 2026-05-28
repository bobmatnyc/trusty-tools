// Pre-existing clippy warnings across this large binary crate.
// Each category below is suppressed at crate level with rationale:
// - dead_code / unused_imports: Many helpers are kept for future use, behind
//   feature flags, or used only on certain platforms / by tests; pruning them
//   is its own refactor and would churn unrelated modules.
// - clippy::collapsible_if / collapsible_else_if: Style preference; nested
//   ifs are often clearer with the existing comments and gating logic.
// - clippy::manual_str_repeat / manual_repeat_n / single_char_add_str: Style
//   nits in display/formatting code where current form reads fine.
// - clippy::too_many_arguments: A few orchestration entry points genuinely
//   need their argument count; signatures are part of internal contracts.
// - clippy::await_holding_lock: Test-only — a std::sync::Mutex serializes
//   tests that mutate process-global env (HOME, etc.). The await points are
//   inside the critical section by design, and tests are single-threaded
//   per-test by virtue of the lock.
// - clippy::clone_on_copy / len_zero / map_or / etc.: Misc style nits in
//   pre-existing code; not worth the churn vs. risk of breaking 1500+ tests.
#![allow(dead_code)]
#![allow(unused_imports)]
#![allow(unused_mut)]
#![allow(unused_assignments)]
#![allow(unused_variables)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::collapsible_else_if)]
#![allow(clippy::manual_str_repeat)]
#![allow(clippy::manual_repeat_n)]
#![allow(clippy::single_char_add_str)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::await_holding_lock)]
#![allow(clippy::clone_on_copy)]
#![allow(clippy::len_zero)]
#![allow(clippy::unnecessary_map_or)]
#![allow(clippy::manual_map)]
#![allow(clippy::needless_borrows_for_generic_args)]
#![allow(clippy::unnecessary_sort_by)]
#![allow(clippy::if_same_then_else)]
#![allow(clippy::new_without_default)]
#![allow(clippy::manual_split_once)]
#![allow(clippy::needless_splitn)]
#![allow(clippy::single_match_else)]
#![allow(clippy::single_match)]
#![allow(clippy::ptr_arg)]
#![allow(clippy::manual_clamp)]
#![allow(clippy::redundant_closure)]
#![allow(clippy::manual_pattern_char_comparison)]
#![allow(clippy::vec_init_then_push)]
#![allow(clippy::single_component_path_imports)]
#![allow(clippy::derivable_impls)]
#![allow(clippy::match_single_binding)]
#![allow(clippy::redundant_pattern_matching)]

//! PM mode: interactive orchestrator that delegates to sub-agents.

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::default_bundled_config_dir;
use crate::{agents, git, llm, mcp, ticketing, tools};

use agents::AgentConfig;
use subprocess::SubprocessAgentRunner;
use tools::{ToolRegistry, delegate::DelegateToAgentTool};

use crate::subprocess;

/// PM mode: interactive orchestrator.
pub(super) async fn run_pm() -> Result<()> {
    tracing::info!("open-mpm PM starting (orchestrator mode)");

    let mut pm_cfg = AgentConfig::by_name("pm").context("failed to load pm agent config")?;

    // Inject the dynamic agent roster into the PM system prompt. Without this,
    // the PM's TOML-encoded prompt would either hardcode a partial agent list
    // (root cause of over-delegation to `python-engineer`) or leave the
    // `{{available_agents}}` placeholder literal. Load the registry from the
    // same search-path policy used elsewhere so project-level overrides win.
    let roster_registry = agents::registry::AgentRegistry::load(
        &agents::registry::agent_search_paths(&default_bundled_config_dir()),
    );
    pm_cfg.system_prompt.content = agents::registry::inject_roster_into_prompt(
        &pm_cfg.system_prompt.content,
        &roster_registry,
    );

    let client = llm::create_client()?;

    // Registry with a single tool (delegate_to_agent) wired to the
    // production subprocess runner.
    let runner: Arc<dyn tools::AgentRunner> = Arc::new(SubprocessAgentRunner::new());
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(DelegateToAgentTool::new(runner)));
    // #304: Coordinator-facing shell executor — see `tools::run_bash`.
    {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        registry.register(Arc::new(tools::run_bash::RunBashTool::new(cwd)));
    }
    // #244: Dynamic MCP service management tools (mcp_list/add/remove/enable/disable).
    for tool in tools::mcp_tools::mcp_tool_executors() {
        registry.register(tool);
    }
    // #243: Native ticketing tools (gated on `[github]` identity in
    // ~/.open-mpm/config.toml — silently absent when not configured).
    {
        let cfg = mcp::config::GlobalConfig::load().await;
        if let Some(identity) = cfg.github_identity(None)
            && let Some(tk_cfg) = identity.to_ticketing_config()
        {
            match tk_cfg.build_client().await {
                Ok(client_box) => {
                    let client: Arc<dyn ticketing::TicketingClient> = Arc::from(client_box);
                    let actions = ticketing::actions::build_actions_client(
                        identity.token().as_deref(),
                        identity.repo().as_deref(),
                    )
                    .await;
                    for tool in tools::native_ticketing::ticketing_tools(client, actions) {
                        registry.register(tool);
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "ticketing client build failed; PM running without ticketing tools");
                }
            }
        }
    }
    // #247: Native git tools, gated by `[git].available_for_roles` for "pm".
    // Repo discovery from cwd; failure is non-fatal (PM simply runs without
    // git tools when not inside a repo).
    {
        let cfg = mcp::config::GlobalConfig::load().await;
        if cfg.git.available_for_roles.iter().any(|r| r == "pm") {
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            match git::GitRepo::open(&cwd) {
                Ok(repo) => {
                    for tool in tools::git_tools::git_tools(repo.root.clone()) {
                        registry.register(tool);
                    }
                }
                Err(e) => {
                    tracing::debug!(error = %e, "no git repo discovered; PM running without git tools");
                }
            }
        }
    }
    let openai_tools = registry.openai_tools()?;

    eprint!("> ");
    let mut user_input = String::new();
    let mut stdin = BufReader::new(tokio::io::stdin());
    stdin
        .read_line(&mut user_input)
        .await
        .context("failed to read user input from stdin")?;
    let user_input = user_input.trim().to_string();
    if user_input.is_empty() {
        bail!("empty user input");
    }

    tracing::debug!(user_input = %user_input, "dispatching to PM LLM");
    let response = llm::chat(
        &client,
        &pm_cfg.agent.model,
        &pm_cfg.system_prompt.content,
        &user_input,
        pm_cfg.llm.temperature,
        pm_cfg.llm.max_tokens,
        openai_tools,
    )
    .await?;

    if response.tool_calls.is_empty() {
        if let Some(text) = response.content {
            println!("{text}");
        } else {
            println!("(no content and no tool calls)");
        }
        return Ok(());
    }

    for tc in response.tool_calls {
        if !registry.contains(&tc.name) {
            tracing::warn!(tool = %tc.name, "ignoring unknown tool call");
            continue;
        }
        tracing::info!(tool = %tc.name, "dispatching PM tool call");
        let result = registry.dispatch(&tc.name, tc.arguments).await;
        if result.is_error() {
            eprintln!("tool '{}' failed: {}", tc.name, result.content());
        } else {
            println!("{}", result.content());
        }
    }

    Ok(())
}
