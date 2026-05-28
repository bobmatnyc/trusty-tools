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

//! PM and sub-agent execution modes, per-agent tool-registry construction, and postmortem triggering.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use async_openai::types::{
    ChatCompletionRequestMessage, ChatCompletionRequestSystemMessageArgs,
    ChatCompletionRequestUserMessageArgs,
};
use chrono;
use clap::Parser;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
// Why: Modules are owned by the `open_mpm` library crate (see src/lib.rs); this
//      binary re-exports them under `crate::` so existing `crate::foo::*` paths
//      throughout this file (and the integration tests) keep resolving without
//      a large sweep. This also gives external agent crates (cto-assistant) a
//      stable library handle to the same `ToolExecutor` / `AgentPlugin` types
//      this binary uses for injection.
// What: One `use open_mpm::foo as foo;` per top-level module. The `pub use`
//       re-export pattern would also work but keeps the binary's surface
//       deliberately small.
// Test: The binary continues to build and run end-to-end via `cargo build`
//       and the existing tmux/REPL tests.
use crate::default_bundled_config_dir;
use crate::{
    adapters, agents, api, ast, build_info, bus, cli, compress, context, ctrl, ctrl_session,
    debugger, docs_index, eval, events, git, identity, init, inspection, intent, interaction_log,
    ipc, llm, local_inference, logging, mcp, memory, mistake_log, perf, plugins, process_tracker,
    progress, rbac, recap, registry, repl, rpc, search, service, session, session_record,
    session_registry, skills, slack, state_writer, subprocess, telegram, ticketing, tm, tmux,
    tools, update, usage, workflow,
};

use memory::{CodeStore, FastEmbedder};
use search::{CodeIndexer, FileWatcher};

use agents::AgentConfig;
use agents::claude_code_runner::{ClaudeCodeAgentRunner, DispatchingAgentRunner};
use agents::harness_protocol::{BASE_PROTOCOL, CLAUDE_CODE_PROTOCOL, FINISH_TASK_PROTOCOL};
use agents::prompt_builder::SystemPromptBuilder;
use build_info::BuildInfo;
use ipc::{IpcMessage, extract_summary, parse_message, serialize_message};
use subprocess::{SubprocessAgentRunner, spawn_subagent_and_run};
use tools::SkillResolver;
use tools::fs_reader::{GrepFilesTool, ListDirTool, ReadFileTool};
#[allow(unused_imports)]
use tools::memory::{MemoryRecallTool, VectorSearchTool};
use tools::phase_audit::PhaseAuditTool;
use tools::shell::ShellExecTool as LocalOpsShellTool;
use tools::skill_loader::{FsSkillResolver, SkillListTool, SkillLoaderTool};
use tools::web_search::{BraveSearchTool, FetchUrlTool};
use tools::write_file::WriteFileTool;
use tools::{ToolRegistry, delegate::DelegateToAgentTool, shell_exec::ShellExecTool};
use workflow::WorkflowEngine;

/// Sub-agent mode: consume one Task, produce one Result/Error, exit.
///
/// Supports two execution paths based on the agent config's system prompt
/// "tools" list (resolved from the agent name):
///   - Agents with tool needs (research, qa, etc.) run a multi-turn loop
///     via `llm::chat_with_tools` with an appropriate `ToolRegistry`.
///   - Plain agents (python-engineer, plan-agent, observe-agent) run a
///     single-shot `llm::chat` with no tools.
pub(super) async fn run_subagent(name: &str) -> Result<()> {
    tracing::info!(agent = %name, "sub-agent starting");

    let mut cfg = AgentConfig::by_name(name)
        .with_context(|| format!("failed to load agent config for '{name}'"))?;

    // #88: Per-call `max_turns` override via `OPEN_MPM_MAX_TURNS`. The wave
    // loop sets this to tighten the turn budget per file (e.g. 20) so a
    // single invocation can't absorb an entire wave's work. Applied after
    // config load and before any use of `cfg.llm.max_turns` so every code
    // path (tool-using + single-shot) honors it.
    // Why: The sub-agent reads the agent TOML (e.g. `code-agent.toml`,
    // `max_turns = 50`) which is correct for legacy/monolithic runs but too
    // loose for per-file wave-loop invocations. Env-var override keeps the
    // TOML as the default while letting the orchestrator enforce a tighter
    // cap without reshaping the `AgentRunner` trait.
    // What: Parses the env var as u32; silently ignores unparseable values
    // so a malformed override can't brick a sub-agent.
    if let Ok(s) = std::env::var("OPEN_MPM_MAX_TURNS")
        && let Ok(v) = s.parse::<u32>()
        && v > 0
    {
        tracing::info!(
            agent = %name,
            original = cfg.llm.max_turns,
            override_to = v,
            "applying OPEN_MPM_MAX_TURNS override"
        );
        cfg.llm.max_turns = v;
    }

    // Qualify bare Claude model ids with `anthropic/` when this sub-agent
    // routes via OpenRouter. Mirrors the PM-side fix in
    // `ctrl::run_pm_task_with_history`; without it, agent TOMLs that ship
    // bare ids (e.g. `claude-haiku-4-5`) get rejected with HTTP 400 by
    // OpenRouter. Centralized in `llm::credentials::qualify_openrouter_model`
    // so every dispatch path uses the same rule.
    if let Some(creds) = llm::credentials::pick_credentials(Some(cfg.agent.runner)) {
        let qualified = llm::credentials::qualify_openrouter_model(&creds, &cfg.agent.model);
        if qualified != cfg.agent.model {
            tracing::debug!(
                agent = %name,
                from = %cfg.agent.model,
                to = %qualified,
                "qualifying bare claude model id for OpenRouter (sub-agent)"
            );
            cfg.agent.model = qualified;
        }
    }

    // #61: Log which endpoint and auth source this agent will use so operators
    // can verify Claude Max OAuth vs API key vs OpenRouter at a glance.
    {
        let ep = cfg.adapter.api_endpoint(cfg.llm.use_anthropic_direct);
        // Strip "https://" prefix and any path after the host for a compact log.
        let host = ep
            .base_url
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap_or(&ep.base_url);
        tracing::info!(
            agent = %name,
            model = %cfg.agent.model,
            endpoint = %host,
            auth = %ep.auth_source,
            "resolved agent endpoint"
        );
    }

    let client = llm::create_client()?;

    // Read stdin for the NDJSON Task line.
    let mut input = String::new();
    tokio::io::stdin()
        .read_to_string(&mut input)
        .await
        .context("failed to read sub-agent stdin")?;
    let first_line = input.lines().next().context("no NDJSON line on stdin")?;
    let msg = parse_message(first_line)?;

    let (task_id, task_text, history, session_reset) = match msg {
        IpcMessage::Task {
            id,
            task,
            history,
            session_reset,
        } => (id, task, history, session_reset),
        other => bail!("sub-agent expected Task message, got: {other:?}"),
    };

    // #51: Persistent-session reset. When the caller sets `session_reset`,
    // the sub-agent must behave as if no prior history exists for this run.
    // We simply ignore any history the caller also sent in that case.
    let effective_history: Option<Vec<session::HistoryMessage>> = if session_reset.unwrap_or(false)
    {
        None
    } else {
        history
    };

    tracing::debug!(task_id = %task_id, agent = %name, "sub-agent processing task");

    // Assemble the effective system prompt in layers:
    //   1. Base prompt from the agent TOML.
    //   2. CLAUDE.md ancestor walk from CWD (project + home instructions).
    //   3. Any resolved skills declared by the agent.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut builder =
        SystemPromptBuilder::new(cfg.system_prompt.content.clone()).walk_project_instructions(&cwd);

    // Harness protocol layers (single source of truth for write_file /
    // finish_task / out_dir / ## Summary rules). Injected between goal block
    // and base TOML prompt. Content is compiled into the binary via
    // `agents::harness_protocol` — the protocol is binary behavior, not user
    // config, so it cannot be disabled by editing files on disk.
    builder = builder.add_harness_layer(BASE_PROTOCOL);
    if matches!(cfg.agent.runner, agents::RunnerKind::ClaudeCode) && !cfg.llm.use_finish_task {
        builder = builder.add_harness_layer(CLAUDE_CODE_PROTOCOL);
    }
    if cfg.llm.use_finish_task {
        builder = builder.add_harness_layer(FINISH_TASK_PROTOCOL);
    }

    if let Some(skills) = &cfg.system_prompt.skills
        && !skills.is_empty()
    {
        let resolver = FsSkillResolver::from_defaults();
        for s in skills {
            if let Some(text) = resolver.resolve(s) {
                let layer = format!("# Skill: {s}\n\n{text}");
                builder = builder.add_skill(layer);
            } else {
                tracing::warn!(agent = %name, skill = %s, "skill not found; skipping");
            }
        }
    }

    // #241: MCP tool descriptions, role-gated. Engineer/coder/qa/ops agents
    // are excluded by `inject_for_roles` in the global config so this is a
    // no-op for them; coordinating roles (ctrl, pm, research, observe) get
    // a Markdown block listing the tools they can call.
    // #244: Use load() (no create-if-absent) so changes made by mcp_* tools
    // in earlier turns are reflected in this prompt build without caching.
    let mcp_cfg = mcp::GlobalConfig::load().await;
    if let Some(section) = mcp_cfg.render_prompt_section(&cfg.agent.role) {
        builder = builder.add_mcp_layer(section);
    }

    // #420: Inject caveman-style output compression fragment from the agent's
    // [compress] output_style field. Defaults to OutputStyle::Full so every
    // agent gets compression unless explicitly set to `output_style = "none"`.
    builder = builder.with_output_style(cfg.compress.output_style);

    let system_prompt_content = builder.build();

    // Optional out_dir for audit tool (from env set by subprocess runner).
    let out_dir = std::env::var_os("OPEN_MPM_OUT_DIR").map(PathBuf::from);
    // #222: Optional code_dir override for tools that write generated source
    // files (code-agent's WriteFileTool). Falls back to out_dir when unset
    // so legacy single-dir runs are unchanged.
    let code_dir = std::env::var_os("OPEN_MPM_CODE_DIR").map(PathBuf::from);

    // #81: Load the legacy skill registry once per sub-agent invocation. Missing
    // `.open-mpm/skills/` is a graceful no-op — the registry just stays empty.
    let skill_registry = Arc::new(
        skills::SkillRegistry::load(&cwd.join(".open-mpm").join("skills"))
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "failed to load skill registry; using empty");
                skills::SkillRegistry::empty()
            }),
    );

    // #170: Load the tag-indexed skill registry (#168) using the same
    // hierarchical search paths as the PM process. This powers tag-ranked
    // `list_skills(tags=[...])` from within this sub-agent. Missing source
    // dirs are a graceful no-op — the registry simply returns empty results.
    let tag_skill_registry = Arc::new(skills::registry::SkillRegistry::load(
        &skills::registry::skill_search_paths(&default_bundled_config_dir()),
    ));

    // Build the per-agent tool registry based on agent name.
    let mut registry = super::tool_registry::build_registry_for_agent(
        name,
        out_dir.as_deref(),
        code_dir.as_deref(),
        skill_registry.clone(),
        tag_skill_registry.clone(),
    );

    // #57: If the agent opts into `use_finish_task`, auto-register the
    // terminal tool. Create a fresh registry when the agent didn't have one
    // (a pure `finish_task`-only agent is still valid).
    if cfg.llm.use_finish_task {
        let reg = registry.get_or_insert_with(ToolRegistry::new);
        reg.register(Arc::new(tools::finish_task::FinishTaskTool::new()));
    }

    let result = if let Some(reg) = registry {
        super::subagent_exec::run_subagent_with_tools(
            &client,
            &cfg,
            &system_prompt_content,
            &task_text,
            reg,
            effective_history.as_deref(),
        )
        .await
    } else {
        super::subagent_exec::run_subagent_single_shot(
            &client,
            &cfg,
            &system_prompt_content,
            &task_text,
            effective_history.as_deref(),
        )
        .await
    };

    let response = match result {
        Ok((content, usage)) => {
            // #27: Extract a summary from the agent's content so downstream
            // workflow phases receive a concise digest via `{{phase_name}}`
            // substitution rather than the full (often huge) output.
            let summary = extract_summary(&content);
            let summary_opt = if summary.is_empty() {
                None
            } else {
                Some(summary)
            };
            // #47: Only attach usage if we actually saw token counts; zero
            // usage would skew perf aggregations (the wire protocol omits
            // absent usage entirely thanks to `skip_serializing_if`).
            let usage_opt = if usage == perf::TokenUsage::default() {
                None
            } else {
                Some(usage)
            };
            IpcMessage::new_result_full(&task_id, content, summary_opt, usage_opt)
        }
        Err(e) => {
            let err_msg = IpcMessage::new_error(&task_id, format!("agent '{name}' failed: {e:#}"));
            let line = serialize_message(&err_msg)?;
            let mut stdout = tokio::io::stdout();
            stdout.write_all(line.as_bytes()).await?;
            stdout.flush().await?;
            return Err(e);
        }
    };

    let line = serialize_message(&response)?;
    let mut stdout = tokio::io::stdout();
    stdout.write_all(line.as_bytes()).await?;
    stdout.flush().await?;
    tracing::info!(agent = %name, "sub-agent complete");
    Ok(())
}
