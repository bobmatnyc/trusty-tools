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
    let mut registry = build_registry_for_agent(
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
        run_subagent_with_tools(
            &client,
            &cfg,
            &system_prompt_content,
            &task_text,
            reg,
            effective_history.as_deref(),
        )
        .await
    } else {
        run_subagent_single_shot(
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

async fn run_subagent_single_shot(
    client: &async_openai::Client<async_openai::config::OpenAIConfig>,
    cfg: &AgentConfig,
    system_prompt: &str,
    task_text: &str,
    history: Option<&[session::HistoryMessage]>,
) -> Result<(String, perf::TokenUsage)> {
    // When the caller provided persistent-session history, we need the full
    // message vector path (system + history... + user). `llm::chat` only
    // takes system+user, so we fall through to the messages-based loop with
    // an empty tool registry in that case.
    if let Some(hist) = history
        && !hist.is_empty()
    {
        // #135: Apply send-time compression (no-op unless [compress] enabled).
        let (hist_compressed, task_compressed) =
            llm::apply_compression(hist.to_vec(), task_text.to_string(), &cfg.compress);

        let system_msg: ChatCompletionRequestMessage =
            ChatCompletionRequestSystemMessageArgs::default()
                .content(system_prompt)
                .build()
                .context("failed to build system message")?
                .into();
        let mut messages: Vec<ChatCompletionRequestMessage> =
            Vec::with_capacity(hist_compressed.len() + 2);
        messages.push(system_msg);
        for h in &hist_compressed {
            messages.push(h.clone().into_typed()?);
        }
        let user_msg: ChatCompletionRequestMessage =
            ChatCompletionRequestUserMessageArgs::default()
                .content(task_compressed.as_str())
                .build()
                .context("failed to build user message")?
                .into();
        messages.push(user_msg);

        // Bedrock-routed sub-agents need AWS profile/region exposed via env vars
        // (mirrors the guard in `run_subagent_with_tools`).
        let _aws_env_guard = if cfg.adapter.provider() == llm::adapter::Provider::Bedrock {
            Some(agents::in_process_runner::BedrockEnvGuard::install(
                cfg.llm.aws_profile.as_deref(),
                cfg.llm.aws_region.as_deref(),
            ))
        } else {
            None
        };

        let (content, usage) = llm::chat_with_tools_gated(
            client,
            &cfg.agent.model,
            &*cfg.adapter,
            messages,
            Arc::new(ToolRegistry::new()),
            cfg.tools.allowed.clone(),
            cfg.llm.temperature,
            cfg.llm.max_tokens,
            2,
            cfg.llm.enable_prompt_caching,
            resolve_tool_choice(cfg.llm.tool_choice, &*cfg.adapter),
            cfg.llm.use_finish_task,
            cfg.llm.use_anthropic_direct,
            &cfg.llm.stop_sequences,
        )
        .await?;
        return Ok((content, usage));
    }

    let response = llm::chat(
        client,
        &cfg.agent.model,
        system_prompt,
        task_text,
        cfg.llm.temperature,
        cfg.llm.max_tokens,
        vec![],
    )
    .await?;
    Ok((
        response
            .content
            .unwrap_or_else(|| "(sub-agent produced no content)".to_string()),
        response.usage,
    ))
}

async fn run_subagent_with_tools(
    client: &async_openai::Client<async_openai::config::OpenAIConfig>,
    cfg: &AgentConfig,
    system_prompt: &str,
    task_text: &str,
    registry: ToolRegistry,
    history: Option<&[session::HistoryMessage]>,
) -> Result<(String, perf::TokenUsage)> {
    let system_msg: ChatCompletionRequestMessage =
        ChatCompletionRequestSystemMessageArgs::default()
            .content(system_prompt)
            .build()
            .context("failed to build system message")?
            .into();

    // #135: Apply send-time compression (no-op unless [compress] enabled).
    // Stored history in the SessionManager is never mutated — only the
    // wire copy we're about to send is.
    let (hist_for_wire, task_for_wire) = llm::apply_compression(
        history.map(|h| h.to_vec()).unwrap_or_default(),
        task_text.to_string(),
        &cfg.compress,
    );

    // #51: If the caller forwarded session history (persistent agent), splice
    // it between the system message and the new user task so the model has
    // the full running dialog.
    let mut messages: Vec<ChatCompletionRequestMessage> = Vec::new();
    messages.push(system_msg);
    for h in &hist_for_wire {
        messages.push(h.clone().into_typed()?);
    }
    let user_msg: ChatCompletionRequestMessage = ChatCompletionRequestUserMessageArgs::default()
        .content(task_for_wire.as_str())
        .build()
        .context("failed to build user message")?
        .into();
    messages.push(user_msg);

    let allowed = cfg.tools.allowed.clone();

    // Bedrock-routed sub-agents need AWS profile/region exposed via env vars
    // so `chat_with_tools_gated` can build the Bedrock client. The in-process
    // runner installs an identical guard; the subprocess path was missing it,
    // which made `bedrock/...` agents fail with the SDK default credential
    // chain (no profile, wrong region).
    let _aws_env_guard = if cfg.adapter.provider() == llm::adapter::Provider::Bedrock {
        Some(agents::in_process_runner::BedrockEnvGuard::install(
            cfg.llm.aws_profile.as_deref(),
            cfg.llm.aws_region.as_deref(),
        ))
    } else {
        None
    };

    let (content, usage) = llm::chat_with_tools_gated(
        client,
        &cfg.agent.model,
        &*cfg.adapter,
        messages,
        Arc::new(registry),
        allowed,
        cfg.llm.temperature,
        cfg.llm.max_tokens,
        cfg.llm.max_turns,
        cfg.llm.enable_prompt_caching,
        resolve_tool_choice(cfg.llm.tool_choice, &*cfg.adapter),
        cfg.llm.use_finish_task,
        cfg.llm.use_anthropic_direct,
        &cfg.llm.stop_sequences,
    )
    .await?;
    Ok((content, usage))
}

/// Translate the TOML-level `ToolChoice` enum into the provider-specific
/// `tool_choice` JSON value using the agent's adapter.
///
/// Why: `agents::ToolChoice` is a small config enum; the actual wire shape
/// depends on the provider family (`{"type":"any"}` vs `"required"`), so we
/// funnel through the adapter here.
/// What: Maps `Auto` → adapter's auto value (usually `"auto"`), `Any` →
/// `tool_choice_any`, `None` → literal JSON `"none"`. Returns `None` when
/// the adapter has no preference (generic providers), letting the chat
/// builder omit the field entirely.
/// Test: Exercised through `main` integration; unit coverage via adapter tests.
fn resolve_tool_choice(
    choice: agents::ToolChoice,
    adapter: &dyn llm::adapter::ModelAdapter,
) -> Option<serde_json::Value> {
    match choice {
        agents::ToolChoice::Auto => adapter.tool_choice_auto(),
        agents::ToolChoice::Any => adapter.tool_choice_any(),
        agents::ToolChoice::None => Some(serde_json::Value::String("none".to_string())),
    }
}

/// Build a tool registry tailored to a specific agent.
///
/// Why: Different agents need different tools (research -> web_search,
/// load_skill; qa -> pytest_exec). Hardcoding the mapping here keeps it
/// discoverable; a later version could drive it from the agent TOML.
/// What: Returns `Some(ToolRegistry)` for agents that use tools, else None.
/// `out_dir`, if present, is used to register `advance_workflow_phase`.
/// Test: Called during `run_subagent`.
pub(super) fn build_registry_for_agent(
    name: &str,
    out_dir: Option<&std::path::Path>,
    code_dir: Option<&std::path::Path>,
    skill_registry: Arc<skills::SkillRegistry>,
    tag_skill_registry: Arc<skills::registry::SkillRegistry>,
) -> Option<ToolRegistry> {
    // #222: When `code_dir` is set and distinct from `out_dir`, the code-agent
    // and any future tool that writes *generated source files* should root at
    // `code_dir` (the user's project tree). All other agents (plan, docs,
    // observe) keep writing artifacts to `out_dir`. When `code_dir` is None
    // we fall back to `out_dir` for full backward compatibility.
    let code_root = code_dir.or(out_dir);
    // #81: `load_skill` and `list_skills` are registered for every agent that
    // builds a registry. The skill registry itself is loaded once per process
    // (empty when `.open-mpm/skills/` is absent, so wiring is safe unconditionally).
    // Per-agent `[tools].allowed` lists still gate whether the agent can call
    // these; agents that omit `allowed` get unrestricted access as before.
    //
    // #170: When a non-empty tag-indexed registry (#168) is available, wire it
    // into `list_skills` so `tags=[...]` returns tag-ranked results. The
    // legacy `SkillRegistry` remains as a fallback for rendering when the
    // tag registry yields nothing and for `load_skill`'s frontmatter-aware
    // body rendering.
    let register_skill_tools = |reg: &mut ToolRegistry| {
        let resolver: Arc<dyn tools::SkillResolver> = Arc::new(FsSkillResolver::from_defaults());
        reg.register(Arc::new(SkillLoaderTool::with_registry(
            resolver.clone(),
            skill_registry.clone(),
        )));
        if !tag_skill_registry.is_empty() {
            reg.register(Arc::new(SkillListTool::with_tag_registry(
                resolver,
                Some(skill_registry.clone()),
                tag_skill_registry.clone(),
            )));
        } else {
            reg.register(Arc::new(SkillListTool::with_registry(
                resolver,
                skill_registry.clone(),
            )));
        }
    };
    // #52: `web_search` and `fetch_url` are registered unconditionally for
    // every agent that builds a registry. The per-agent `[tools].allowed`
    // list in TOML governs who is actually permitted to call them; the tool
    // itself degrades gracefully when BRAVE_API_KEY is unset.
    fn register_web_tools(reg: &mut ToolRegistry) {
        reg.register(Arc::new(BraveSearchTool::from_env()));
        reg.register(Arc::new(FetchUrlTool::new()));
    }

    /// #199: `wait_ms` and `poll_until` are universal async-flow tools — every
    /// agent benefits from being able to back off or wait for an external
    /// signal. Per-agent TOML allowlists still gate actual usage.
    fn register_timer_tools(reg: &mut ToolRegistry) {
        reg.register(Arc::new(tools::timer::WaitMsTool::new()));
        reg.register(Arc::new(tools::timer::PollUntilTool::new()));
    }

    // #53: `memory_recall` and `vector_search` are research aids and are
    // registered alongside web tools for any agent that benefits from them.
    // Both degrade gracefully when their underlying stores are missing, so
    // registering them is safe even when the project hasn't been indexed.
    //
    // #71: `memory_search` is a hybrid (vector + BM25) retriever with LLM
    // consolidation over the `.open-mpm/history/` turn log. Added alongside
    // the existing memory tools for the same gracefully-degrading rationale.
    fn register_memory_tools(reg: &mut ToolRegistry) {
        reg.register(Arc::new(MemoryRecallTool::new()));
        reg.register(Arc::new(VectorSearchTool::new()));
        reg.register(Arc::new(tools::memory_search::MemorySearchTool::from_env()));
    }

    match name {
        "research-agent" => {
            // Unified read-only investigator: web tools + memory/vector tools +
            // skills + read-only filesystem exploration. Merged with the former
            // explorer-agent so research-agent is the single "find out" agent.
            // All tools here are side-effect free; per-agent TOML allowlist
            // governs which are actually callable.
            let mut reg = ToolRegistry::new();
            register_web_tools(&mut reg);
            register_memory_tools(&mut reg);
            register_skill_tools(&mut reg);
            register_timer_tools(&mut reg);
            reg.register(Arc::new(ReadFileTool::new()));
            reg.register(Arc::new(ListDirTool::new()));
            reg.register(Arc::new(GrepFilesTool::new()));
            // #373: research benefits from structural analysis tools.
            for t in tools::analysis::analysis_tools() {
                reg.register(t);
            }
            if let Some(dir) = out_dir {
                reg.register(Arc::new(PhaseAuditTool::new(dir.to_path_buf())));
            }
            Some(reg)
        }
        "analysis-agent" => {
            // #373: code-quality analyst agent. Registers the full analysis
            // tool bundle (complexity, smells, hotspots, dependency cycles,
            // call graphs) plus read-only filesystem + skills + memory so it
            // can dig into specific files when an automated metric flags one.
            let mut reg = ToolRegistry::new();
            register_memory_tools(&mut reg);
            register_skill_tools(&mut reg);
            reg.register(Arc::new(ReadFileTool::new()));
            reg.register(Arc::new(ListDirTool::new()));
            reg.register(Arc::new(GrepFilesTool::new()));
            for t in tools::analysis::analysis_tools() {
                reg.register(t);
            }
            if let Some(dir) = out_dir {
                reg.register(Arc::new(PhaseAuditTool::new(dir.to_path_buf())));
            }
            Some(reg)
        }
        "code-agent" => {
            // Code generation agent. Gets write_file so it can emit files
            // directly as tool calls (avoids plain-text-mid-task retries for
            // large multi-file outputs). Also gets read-only exploration tools
            // so it can inspect existing code and the phase-audit tool for
            // workflow phase management.
            let mut reg = ToolRegistry::new();
            register_skill_tools(&mut reg);
            register_timer_tools(&mut reg);
            reg.register(Arc::new(ReadFileTool::new()));
            reg.register(Arc::new(ListDirTool::new()));
            reg.register(Arc::new(GrepFilesTool::new()));
            // #222: write_file roots at `code_root` (= code_dir when set,
            // else out_dir) so generated source lands in the user's project
            // tree when --project-dir is used. PhaseAuditTool stays anchored
            // at out_dir because the audit trail is an artifact.
            if let Some(dir) = code_root {
                // #88: If `OPEN_MPM_ASSIGNED_FILE` is set, we're inside a
                // per-file wave-loop invocation and must restrict writes to
                // that single path. Otherwise fall through to the legacy
                // unrestricted behavior (full code_root tree writable).
                let mut write_tool = WriteFileTool::new(dir.to_path_buf());
                if let Some(assigned) = std::env::var_os("OPEN_MPM_ASSIGNED_FILE") {
                    write_tool = write_tool.with_allowed_path(PathBuf::from(assigned));
                }
                reg.register(Arc::new(write_tool));
            } else {
                let fallback = std::env::current_dir().unwrap_or_default();
                reg.register(Arc::new(WriteFileTool::new(fallback)));
            }
            if let Some(dir) = out_dir {
                reg.register(Arc::new(PhaseAuditTool::new(dir.to_path_buf())));
            }
            Some(reg)
        }
        "plan-agent" => {
            // #53: planners benefit from memory_recall + vector_search to
            // ground implementation plans in existing code/decisions.
            // #87: plan-agent also gets write_file (scoped to out_dir) so it
            // can emit stub files and assignments.json for interface-first
            // decomposition. When out_dir is absent we fall back to CWD so
            // the tool remains discoverable in schemas.
            let mut reg = ToolRegistry::new();
            register_memory_tools(&mut reg);
            register_skill_tools(&mut reg);
            register_timer_tools(&mut reg);
            if let Some(dir) = out_dir {
                reg.register(Arc::new(WriteFileTool::new(dir.to_path_buf())));
                reg.register(Arc::new(PhaseAuditTool::new(dir.to_path_buf())));
            } else {
                let fallback = std::env::current_dir().unwrap_or_default();
                reg.register(Arc::new(WriteFileTool::new(fallback)));
            }
            Some(reg)
        }
        "qa-agent" => {
            let mut reg = ToolRegistry::new();
            register_web_tools(&mut reg);
            // #71: memory tools so QA can recall prior decisions / failures.
            register_memory_tools(&mut reg);
            register_skill_tools(&mut reg);
            register_timer_tools(&mut reg);
            reg.register(Arc::new(ShellExecTool::new()));
            if let Some(dir) = out_dir {
                reg.register(Arc::new(PhaseAuditTool::new(dir.to_path_buf())));
            }
            Some(reg)
        }
        "local-ops-agent" => {
            // #77: Local operations agent. Registers a permissive (allowlisted)
            // shell executor plus the read-only filesystem tools so the agent
            // can run commands and verify their effects without mutating
            // source files. `finish_task` is auto-registered elsewhere when
            // `use_finish_task = true` in the agent TOML.
            let mut reg = ToolRegistry::new();
            let work_dir = std::env::current_dir().unwrap_or_default();
            reg.register(Arc::new(LocalOpsShellTool::new(work_dir)));
            reg.register(Arc::new(ReadFileTool::new()));
            reg.register(Arc::new(ListDirTool::new()));
            reg.register(Arc::new(GrepFilesTool::new()));
            register_skill_tools(&mut reg);
            if let Some(dir) = out_dir {
                reg.register(Arc::new(PhaseAuditTool::new(dir.to_path_buf())));
            }
            Some(reg)
        }
        "docs-agent" => {
            // #82: Documentation specialist. Reads generated code (read_file /
            // list_dir / grep_files) and writes docs (write_file) scoped to
            // the workflow's out_dir. `finish_task` is auto-registered
            // elsewhere via `use_finish_task = true` in the agent TOML.
            let mut reg = ToolRegistry::new();
            register_skill_tools(&mut reg);
            reg.register(Arc::new(ReadFileTool::new()));
            reg.register(Arc::new(ListDirTool::new()));
            reg.register(Arc::new(GrepFilesTool::new()));
            if let Some(dir) = out_dir {
                reg.register(Arc::new(WriteFileTool::new(dir.to_path_buf())));
                reg.register(Arc::new(PhaseAuditTool::new(dir.to_path_buf())));
            } else {
                // Even without out_dir, register a WriteFileTool rooted at CWD
                // so the tool is discoverable in schemas. In practice workflow
                // mode always provides out_dir; direct mode may not.
                let fallback = std::env::current_dir().unwrap_or_default();
                reg.register(Arc::new(WriteFileTool::new(fallback)));
            }
            Some(reg)
        }
        _ => {
            // #81: Agents without a dedicated tool branch still benefit from
            // skill discovery/loading. Build a minimal registry that just
            // exposes `list_skills` and `load_skill`, plus the phase-audit
            // tool when a workflow out_dir is available. Per-agent allowlists
            // still govern whether any of these can actually be called.
            let mut reg = ToolRegistry::new();
            register_skill_tools(&mut reg);
            if let Some(dir) = out_dir {
                reg.register(Arc::new(PhaseAuditTool::new(dir.to_path_buf())));
            }
            Some(reg)
        }
    }
}

/// #186: Spawn the postmortem-agent subprocess for a session. Used both by
/// the auto-trigger path (after a workflow run with logged mistakes) and the
/// `postmortem` CLI subcommand.
///
/// Why: Centralizing dispatch keeps the construction of the task prompt,
/// agent name, and config dir in one place; both callers want the agent to
/// inspect the local `.open-mpm/state/mistakes/<session>.jsonl` file.
/// What: Builds a SubprocessAgentRunner pointed at the project's bundled
/// agents directory, hands it a task that names the session id and the
/// file path, and prints the resulting agent output to stderr.
/// Test: Manual; covered indirectly by the auto-trigger end-to-end flow.
pub(super) async fn trigger_postmortem(project_root: &Path, session_id: &str) -> Result<()> {
    use tools::AgentRunner;
    let agents_config_dir = project_root.join(".open-mpm").join("agents");
    let log_path = project_root
        .join(".open-mpm")
        .join("state")
        .join("mistakes")
        .join(format!("{session_id}.jsonl"));
    let task = format!(
        "Analyze the mistake log at {} for session {} and produce a postmortem report following your standard format. Categorize each failure, apply fixes you are confident about, and recommend follow-ups.",
        log_path.display(),
        session_id
    );
    let runner = subprocess::SubprocessAgentRunner::new().with_config_dir(Some(agents_config_dir));
    let output = runner.run("postmortem-agent", &task).await?;
    eprintln!(
        "\n=== Postmortem Report ({session_id}) ===\n{}",
        output.content
    );
    Ok(())
}

/// #186: `open-mpm postmortem [--session <id>] [--last N]` subcommand.
///
/// Why: Operators want to invoke postmortem analysis on demand — either on
/// a specific failed session or on the recent global error stream — without
/// running a full workflow.
/// What: Parses --session and --last flags, dispatches to either
/// `trigger_postmortem` or feeds the recent global mistakes inline.
/// Test: Manual smoke (`open-mpm postmortem --last 5`); the helper logic is
/// unit-tested via `MistakeLog` directly.
pub(super) async fn run_postmortem_subcommand(args: &[String]) -> Result<()> {
    let mut session: Option<String> = None;
    let mut last: usize = 20;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--session" => {
                session = args.get(i + 1).cloned();
                i += 2;
            }
            "--last" => {
                if let Some(v) = args.get(i + 1).and_then(|s| s.parse::<usize>().ok()) {
                    last = v;
                }
                i += 2;
            }
            _ => i += 1,
        }
    }

    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    if let Some(sid) = session {
        return trigger_postmortem(&project_root, &sid).await;
    }

    // No --session: feed the last N global mistakes to the agent inline.
    let recent = mistake_log::MistakeLog::read_recent_global(last)?;
    if recent.is_empty() {
        println!("(no mistakes recorded)");
        return Ok(());
    }
    let payload = serde_json::to_string_pretty(&recent)?;
    let task = format!(
        "Analyze these {} recent agent mistakes and produce a postmortem report:\n\n{}",
        recent.len(),
        payload
    );
    use tools::AgentRunner;
    let agents_config_dir = project_root.join(".open-mpm").join("agents");
    let runner = subprocess::SubprocessAgentRunner::new().with_config_dir(Some(agents_config_dir));
    let output = runner.run("postmortem-agent", &task).await?;
    println!("{}", output.content);
    Ok(())
}

#[cfg(test)]
mod registry_tests {
    use super::*;

    fn empty_skill_registry() -> Arc<skills::SkillRegistry> {
        Arc::new(skills::SkillRegistry::empty())
    }

    fn empty_tag_registry() -> Arc<skills::registry::SkillRegistry> {
        Arc::new(skills::registry::SkillRegistry::empty())
    }

    #[test]
    fn research_agent_registry_has_web_tools() {
        let reg = build_registry_for_agent(
            "research-agent",
            None,
            None,
            empty_skill_registry(),
            empty_tag_registry(),
        )
        .expect("research-agent builds a registry");
        assert!(
            reg.contains("web_search"),
            "web_search missing from research-agent registry"
        );
        assert!(
            reg.contains("fetch_url"),
            "fetch_url missing from research-agent registry"
        );
    }

    #[test]
    fn research_agent_registry_has_memory_tools() {
        // #53: memory_recall + vector_search registered for the research agent.
        let reg = build_registry_for_agent(
            "research-agent",
            None,
            None,
            empty_skill_registry(),
            empty_tag_registry(),
        )
        .expect("research-agent builds a registry");
        assert!(reg.contains("memory_recall"), "memory_recall missing");
        assert!(reg.contains("vector_search"), "vector_search missing");
    }

    #[test]
    fn research_agent_registry_has_readonly_fs_tools() {
        // Merged from the former explorer-agent: research-agent is now the
        // single "find out" agent and must be able to read/grep the codebase.
        let reg = build_registry_for_agent(
            "research-agent",
            None,
            None,
            empty_skill_registry(),
            empty_tag_registry(),
        )
        .expect("research-agent builds a registry");
        assert!(reg.contains("read_file"), "read_file missing");
        assert!(reg.contains("list_dir"), "list_dir missing");
        assert!(reg.contains("grep_files"), "grep_files missing");
    }

    #[test]
    fn plan_agent_registry_has_memory_tools() {
        // #53: plan-agent gets memory_recall + vector_search so it can ground
        // plans in existing code / project knowledge.
        let reg = build_registry_for_agent(
            "plan-agent",
            None,
            None,
            empty_skill_registry(),
            empty_tag_registry(),
        )
        .expect("plan-agent builds a registry");
        assert!(reg.contains("memory_recall"), "memory_recall missing");
        assert!(reg.contains("vector_search"), "vector_search missing");
    }

    #[test]
    fn all_known_agents_get_skill_tools() {
        // #81: every agent that builds a registry should have load_skill and
        // list_skills available, regardless of whether the skill registry is
        // empty or populated. Per-agent `[tools].allowed` still controls which
        // tools are callable at runtime.
        for agent in [
            "research-agent",
            "plan-agent",
            "qa-agent",
            "local-ops-agent",
            "docs-agent",
            // Unknown agent name: default branch also registers skill tools.
            "unknown-agent",
        ] {
            let reg = build_registry_for_agent(
                agent,
                None,
                None,
                empty_skill_registry(),
                empty_tag_registry(),
            )
            .unwrap_or_else(|| panic!("{agent} should get a registry"));
            assert!(reg.contains("load_skill"), "{agent}: load_skill missing");
            assert!(reg.contains("list_skills"), "{agent}: list_skills missing");
        }
    }

    #[test]
    fn plan_agent_registry_has_write_file_tool() {
        // #87: plan-agent gets write_file so it can emit stub files and
        // assignments.json for interface-first decomposition.
        let reg = build_registry_for_agent(
            "plan-agent",
            None,
            None,
            empty_skill_registry(),
            empty_tag_registry(),
        )
        .expect("plan-agent builds a registry");
        assert!(
            reg.contains("write_file"),
            "write_file missing from plan-agent registry"
        );
    }

    #[test]
    fn docs_agent_registry_has_write_and_read_tools() {
        // #82: docs-agent gets write_file + read-only exploration tools so it
        // can inspect generated code and emit documentation files.
        let reg = build_registry_for_agent(
            "docs-agent",
            None,
            None,
            empty_skill_registry(),
            empty_tag_registry(),
        )
        .expect("docs-agent builds a registry");
        assert!(reg.contains("write_file"), "write_file missing");
        assert!(reg.contains("read_file"), "read_file missing");
        assert!(reg.contains("list_dir"), "list_dir missing");
        assert!(reg.contains("grep_files"), "grep_files missing");
    }

    #[tokio::test]
    async fn list_skills_uses_tag_registry_when_wired() {
        // #170: When `build_registry_for_agent` is called with a non-empty
        // tag-indexed registry, the resulting `list_skills` tool must return
        // tag-ranked JSON (not the legacy float-score format).
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("fastapi.md"),
            "---\nname: fastapi\ndescription: async routes\ntags: [python, fastapi]\n---\nbody\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("rust.md"),
            "---\nname: rust\ndescription: rust idioms\ntags: [rust]\n---\nbody\n",
        )
        .unwrap();

        let tag_reg = Arc::new(skills::registry::SkillRegistry::load(&[dir
            .path()
            .to_path_buf()]));
        assert!(!tag_reg.is_empty(), "sanity: tag registry loaded skills");

        let reg = build_registry_for_agent(
            "research-agent",
            None,
            None,
            empty_skill_registry(),
            tag_reg,
        )
        .expect("research-agent builds a registry");
        assert!(reg.contains("list_skills"));

        let result = reg
            .dispatch("list_skills", serde_json::json!({"tags": ["python"]}))
            .await;
        let content = result.content();
        assert!(
            content.contains("\"fastapi\""),
            "expected fastapi in tag-ranked output, got: {content}"
        );
        assert!(
            content.contains("\"match_score\""),
            "expected tag-registry JSON (match_score field), got: {content}"
        );
        assert!(
            !content.contains("\"rust\""),
            "rust has no 'python' tag and must be filtered out: {content}"
        );
    }

    #[tokio::test]
    async fn list_skills_falls_back_to_legacy_when_tag_registry_empty() {
        // #170: Wiring preserves legacy behavior when the tag registry is
        // empty (no `.open-mpm/skills/` configured). The tool must still
        // register and return a non-panicking response.
        let reg = build_registry_for_agent(
            "research-agent",
            None,
            None,
            empty_skill_registry(),
            empty_tag_registry(),
        )
        .expect("research-agent builds a registry");
        assert!(reg.contains("list_skills"));
        let result = reg.dispatch("list_skills", serde_json::json!({})).await;
        // Empty legacy + empty tag registry yields the resolver fallback
        // string; just assert the call succeeds without panicking.
        let _ = result.content();
    }

    #[tokio::test]
    async fn web_search_without_api_key_returns_graceful_error() {
        // Ensure no key is set for this scope.
        // SAFETY: removing an env var in a test; other tests do not rely on
        // BRAVE_API_KEY being set. The graceful-error path is what we assert.
        unsafe {
            std::env::remove_var("BRAVE_API_KEY");
        }
        let tool = BraveSearchTool::from_env();
        use tools::ToolExecutor;
        let out = tool
            .execute(serde_json::json!({"query": "rust async"}))
            .await;
        assert!(
            out.is_error(),
            "expected an error result when BRAVE_API_KEY is unset"
        );
        assert!(
            out.content().contains("BRAVE_API_KEY"),
            "error should mention BRAVE_API_KEY, got: {}",
            out.content()
        );
    }
}
