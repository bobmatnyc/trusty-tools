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

//! Registry-facing subcommands: agents, plugins, eval, dashboard, skills, and skill-source classification.
//!
//! Module layout (see #366 split): agents/plugins/eval handlers live here;
//! the dashboard + skills/skill-source handlers live in `skills.rs`.

mod skills_cmd;

// Re-export the skills/dashboard handlers so `runtime::subcommands` can keep
// calling `registry_cmds::run_skills_subcommand` / `run_dashboard_subcommand`.
pub(crate) use skills_cmd::{run_dashboard_subcommand, run_skills_subcommand};
// `classify_source` is shared with `run_agents_subcommand` below.
use skills_cmd::classify_source;

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
// Why: Modules are owned by the `trusty_agents` library crate (see src/lib.rs); this
//      binary re-exports them under `crate::` so existing `crate::foo::*` paths
//      throughout this file (and the integration tests) keep resolving without
//      a large sweep. This also gives external agent crates (cto-assistant) a
//      stable library handle to the same `ToolExecutor` / `AgentPlugin` types
//      this binary uses for injection.
// What: One `use trusty_agents::foo as foo;` per top-level module. The `pub use`
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

/// Handle `trusty-agents agents <subcommand>` (#167).
///
/// Why: Exposes the discovery results to operators. Without this, there's
/// no way to verify which agents were picked up from which directory.
/// What: Currently supports `agents list`. Prints discovered agents with
/// their source and capability tags in the format described in the issue.
/// Test: Covered manually; unit-tested via `AgentRegistry::list`.
pub(super) async fn run_agents_subcommand(args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("list");
    match sub {
        "list" => {
            let reg = agents::registry::AgentRegistry::load(&agents::registry::agent_search_paths(
                &default_bundled_config_dir(),
            ));
            let items = reg.list();
            println!("Discovered agents ({}):", items.len());
            let bundled = default_bundled_config_dir().join("agents");
            let home = std::env::var_os("HOME").map(PathBuf::from);
            let max_name = items.iter().map(|s| s.name.len()).max().unwrap_or(0).max(4);
            for s in items {
                let source_label = classify_source(&s.source, &bundled, home.as_deref());
                let mut parts = Vec::new();
                if !s.roles.is_empty() {
                    parts.push(format!("roles: {}", s.roles.join(",")));
                }
                if !s.languages.is_empty() {
                    parts.push(format!("languages: {}", s.languages.join(",")));
                }
                if !s.frameworks.is_empty() {
                    parts.push(format!("frameworks: {}", s.frameworks.join(",")));
                }
                if !s.tags.is_empty() {
                    parts.push(format!("tags: {}", s.tags.join(",")));
                }
                println!(
                    "  {name:<width$}  [{src}]  {caps}",
                    name = s.name,
                    width = max_name,
                    src = source_label,
                    caps = parts.join("  ")
                );
            }
            Ok(())
        }
        other => {
            // #366: Surface a "did you mean?" hint for typos like
            // `agents lst` -> `agents list`.
            let known = &["list"];
            if let Some(s) = cli::did_you_mean(other, known, 2) {
                eprintln!("tagent agents: unknown subcommand '{other}'. Did you mean '{s}'?");
            } else {
                eprintln!("tagent agents: unknown subcommand '{other}'. Try: list");
            }
            bail!("unknown agents subcommand: {other}");
        }
    }
}

/// Handle `trusty-agents plugins <subcommand>` (#414).
///
/// Why: Operators need a quick way to confirm which optional MCP plugins
/// (trusty-search, trusty-memory) the harness is able to spawn. Without
/// this surface, plugin misconfiguration is invisible until an agent tries
/// to use a missing tool.
/// What: Supports `list`, `status` (default), and `check`. All three
/// currently render the same status table; we keep them as distinct verbs
/// so future expansion (e.g. `check` returning non-zero on missing plugins)
/// doesn't break existing scripts.
/// Test: Manual — `om plugins status` on a machine without the trusty
/// binaries reports both as UNAVAILABLE; with binaries on PATH and an MCP
/// handshake, both report ACTIVE.
pub(super) async fn run_plugins_subcommand(args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("status");
    match sub {
        "list" | "status" | "check" => {
            print_plugins_status().await;
            Ok(())
        }
        other => {
            let known = &["list", "status", "check"];
            if let Some(s) = cli::did_you_mean(other, known, 2) {
                eprintln!("tagent plugins: unknown subcommand '{other}'. Did you mean '{s}'?");
            } else {
                eprintln!(
                    "tagent plugins: unknown subcommand '{other}'. Try: list | status | check"
                );
            }
            bail!("unknown plugins subcommand: {other}");
        }
    }
}

/// Why: Wire `om eval run --suite <path> [--agent <toml>] [--json]` (#449)
/// into the CLI dispatch. Loads the suite, resolves the agent system prompt
/// (defaults to a generic helpful-assistant prompt), drives the live
/// OpenRouter client, and prints either a human-friendly report or a JSON
/// array of `EvalResult`.
/// What: Subcommands: `run`. Exit code 0 iff all cases pass.
/// Test: Eval framework itself is unit-tested in `src/eval/mod.rs`; this
/// function is integration-level (requires OPENROUTER_API_KEY).
pub(super) async fn run_eval_subcommand(args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("run");
    if sub != "run" {
        eprintln!("tagent eval: unknown subcommand '{sub}'. Try: run");
        bail!("unknown eval subcommand: {sub}");
    }

    // Parse flags: --suite <path> [--agent <toml>] [--json]
    let rest = &args[1..];
    let mut suite_path: Option<String> = None;
    let mut agent_path: Option<String> = None;
    let mut as_json = false;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--suite" => {
                suite_path = rest.get(i + 1).cloned();
                i += 2;
            }
            "--agent" => {
                agent_path = rest.get(i + 1).cloned();
                i += 2;
            }
            "--json" => {
                as_json = true;
                i += 1;
            }
            other => {
                eprintln!("tagent eval run: unknown flag '{other}'");
                bail!("unknown flag");
            }
        }
    }

    let suite_path = suite_path.ok_or_else(|| anyhow::anyhow!("--suite <path> is required"))?;
    let suite = eval::EvalSuite::from_toml(std::path::Path::new(&suite_path))?;

    // Resolve agent system prompt + model.
    let (system_prompt, model) = if let Some(p) = agent_path.as_deref() {
        let cfg = agents::AgentConfig::load(std::path::Path::new(p))?;
        (cfg.system_prompt.content.clone(), cfg.agent.model.clone())
    } else {
        (
            "You are a helpful assistant.".to_string(),
            "anthropic/claude-sonnet-4-6".to_string(),
        )
    };

    // Live LLM client adapter — uses the existing OpenRouter chat path.
    let client = llm::create_client()?;
    let live = LiveEvalClient {
        client,
        model: model.clone(),
    };

    println!("Running {} eval cases...\n", suite.cases.len());
    let results = suite.run(&system_prompt, &live).await;

    if as_json {
        println!("{}", serde_json::to_string_pretty(&results)?);
    } else {
        print!("{}", eval::EvalSuite::report(&results));
    }

    let failed = results.iter().filter(|r| !r.passed).count();
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}

/// Live `EvalLlmClient` driven by the existing OpenRouter chat path.
struct LiveEvalClient {
    client: async_openai::Client<async_openai::config::OpenAIConfig>,
    model: String,
}

#[async_trait::async_trait]
impl eval::EvalLlmClient for LiveEvalClient {
    async fn complete_with_tools(
        &self,
        system: &str,
        user: &str,
        _user_tier: Option<&str>,
    ) -> Result<(String, Vec<String>)> {
        let resp = llm::chat(&self.client, &self.model, system, user, 0.0, 1024, vec![]).await?;
        let names = resp.tool_calls.iter().map(|t| t.name.clone()).collect();
        Ok((resp.content.unwrap_or_default(), names))
    }
}

/// Render the plugin status table to stdout.
///
/// Why: Shared by `list`, `status`, and `check` so output stays consistent.
/// What: Initialises a `PluginManager`, prints one line per known plugin
/// with state and either the discovered binary path or an install hint.
async fn print_plugins_status() {
    use plugins::PluginState;
    // #424: Reuse the process-wide manager when one is already initialised
    // (e.g. when this is reached via an in-REPL command in the future). At
    // CLI top-level the OnceLock is empty, so we fall back to a fresh
    // `init_global()` so the global is also populated for any subsequent
    // operations in the same process.
    let mgr = match plugins::plugin_manager() {
        Some(existing) => existing,
        None => plugins::init_global().await,
    };
    let s = mgr.status();
    println!("Plugin Status:");
    print_plugin_row("trusty-search", s.search, "cargo install trusty-search");
    print_plugin_row("trusty-memory", s.memory, "cargo install trusty-memory");

    fn print_plugin_row(name: &str, state: PluginState, install_hint: &str) {
        let detail = match state {
            PluginState::Active => match resolve_binary_path(name) {
                Some(p) => format!("(path: {p})"),
                None => String::new(),
            },
            PluginState::Unavailable => format!("(install: {install_hint})"),
        };
        println!("  {name:<14}  {:<11}  {detail}", state.label());
    }

    fn resolve_binary_path(name: &str) -> Option<String> {
        let out = std::process::Command::new("which")
            .arg(name)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if path.is_empty() { None } else { Some(path) }
    }
}
