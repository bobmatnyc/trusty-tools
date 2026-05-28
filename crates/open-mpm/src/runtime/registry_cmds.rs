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

/// Handle `open-mpm agents <subcommand>` (#167).
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
                eprintln!("open-mpm agents: unknown subcommand '{other}'. Did you mean '{s}'?");
            } else {
                eprintln!("open-mpm agents: unknown subcommand '{other}'. Try: list");
            }
            bail!("unknown agents subcommand: {other}");
        }
    }
}

/// Handle `open-mpm plugins <subcommand>` (#414).
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
                eprintln!("open-mpm plugins: unknown subcommand '{other}'. Did you mean '{s}'?");
            } else {
                eprintln!(
                    "open-mpm plugins: unknown subcommand '{other}'. Try: list | status | check"
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
        eprintln!("open-mpm eval: unknown subcommand '{sub}'. Try: run");
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
                eprintln!("open-mpm eval run: unknown flag '{other}'");
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

/// Handle `open-mpm start [--port <port>]` (#403).
///
/// Why: Friendly subcommand alias for `--service start` so users get the
/// daemon-management UX they expect from CLIs like `nginx start` or
/// `docker start`. Reuses `service::start_service` so behaviour and pid-file
/// semantics stay identical.
/// What: Parses optional `--port <u16>` (default 8765 to align with the
/// open-mpm web UI port), polls `/api/health` for up to 10s, prints PID +
/// port on success.
/// Test: `om start` then `om status` then `om stop` against a clean repo.
/// Handle `open-mpm dashboard` (#442).
///
/// Why: Surface the Tauri desktop UI behind a friendly subcommand so users
/// can run `om dashboard` without remembering build paths. We probe a small
/// set of candidate locations (release-first, then debug) and spawn the
/// binary detached if found.
/// What: Tries `<om_dir>/../../ui/src-tauri/target/release/open-mpm-ui`,
/// then `<cwd>/ui/src-tauri/target/release/open-mpm-ui`, then the debug
/// equivalent. On hit: spawns + exits 0. On miss: prints a build hint and
/// exits 1.
/// Test: Manual — `om dashboard` should pop the GUI when built; the error
/// path is exercised by deleting the binaries and re-running.
pub(super) async fn run_dashboard_subcommand(args: &[String]) -> Result<()> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("Usage: open-mpm dashboard|dash");
        println!();
        println!("Launches the Tauri desktop GUI for open-mpm.");
        return Ok(());
    }
    if !args.is_empty() {
        bail!("`dashboard` takes no arguments (got {:?})", args);
    }

    // Candidate paths, in priority order: release first (installed/used),
    // then cwd-relative release, then cwd-relative debug.
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(exe_dir) = exe.parent()
    {
        // `<exe_dir>/../../ui/src-tauri/target/release/open-mpm-ui`
        candidates.push(
            exe_dir
                .join("..")
                .join("..")
                .join("ui")
                .join("src-tauri")
                .join("target")
                .join("release")
                .join("open-mpm-ui"),
        );
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(
            cwd.join("ui")
                .join("src-tauri")
                .join("target")
                .join("release")
                .join("open-mpm-ui"),
        );
        candidates.push(
            cwd.join("ui")
                .join("src-tauri")
                .join("target")
                .join("debug")
                .join("open-mpm-ui"),
        );
    }

    let found = candidates.into_iter().find(|p| p.is_file());
    let Some(binary) = found else {
        eprintln!("Dashboard UI not built. Run: cd ui && npm run tauri:build");
        std::process::exit(1);
    };

    println!("Launching dashboard: {}", binary.display());
    match tokio::process::Command::new(&binary).spawn() {
        Ok(_child) => {
            // Detach: drop the child handle so we don't wait for it. The
            // GUI runs independently of the `om` shell.
            Ok(())
        }
        Err(e) => {
            eprintln!("failed to launch dashboard: {e}");
            std::process::exit(1);
        }
    }
}

/// Handle `open-mpm skills <subcommand>` (#168).
///
/// Why: Gives operators visibility into which skills were discovered, from
/// where, and lets them verify tag-based lookup before delegating. Without
/// this, the registry is invisible.
/// What: Supports `skills list [--tag <tag>]`. Without `--tag`, prints every
/// discovered skill with source label + tags. With `--tag <tag>` (repeatable),
/// filters + ranks by tag-overlap score.
/// Test: Covered manually; unit-tested via `SkillRegistry::find_by_tags`.
pub(super) async fn run_skills_subcommand(args: &[String]) -> Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("list");
    match sub {
        "list" => {
            // Collect zero-or-more `--tag <val>` pairs.
            let mut tags: Vec<String> = Vec::new();
            let mut i = 1; // args[0] == "list"
            while i < args.len() {
                if args[i] == "--tag" {
                    if let Some(v) = args.get(i + 1) {
                        tags.push(v.clone());
                        i += 2;
                        continue;
                    } else {
                        bail!("--tag requires a value");
                    }
                }
                i += 1;
            }

            let reg = skills::registry::SkillRegistry::load(&skills::registry::skill_search_paths(
                &default_bundled_config_dir(),
            ));
            let items: Vec<&skills::registry::SkillMeta> = if tags.is_empty() {
                reg.list()
            } else {
                let refs: Vec<&str> = tags.iter().map(String::as_str).collect();
                reg.find_by_tags(&refs)
            };

            if tags.is_empty() {
                println!("Discovered skills ({}):", items.len());
            } else {
                println!(
                    "Skills matching tags [{}] ({}):",
                    tags.join(","),
                    items.len()
                );
            }

            let bundled = default_bundled_config_dir().join("skills");
            let home = std::env::var_os("HOME").map(PathBuf::from);
            let max_name = items.iter().map(|s| s.name.len()).max().unwrap_or(0).max(4);
            let max_src = items
                .iter()
                .map(|s| classify_skill_source(&s.source_path, &bundled, home.as_deref()).len())
                .max()
                .unwrap_or(0)
                .max(8);
            for s in items {
                let source_label = classify_skill_source(&s.source_path, &bundled, home.as_deref());
                let score_prefix = if tags.is_empty() {
                    String::new()
                } else {
                    let refs: Vec<&str> = tags.iter().map(String::as_str).collect();
                    format!("score={}  ", reg.tag_overlap_score(&s.name, &refs))
                };
                println!(
                    "  {name:<nw$}  [{src:<sw$}]  {score}tags: {tags}",
                    name = s.name,
                    nw = max_name,
                    src = source_label,
                    sw = max_src,
                    score = score_prefix,
                    tags = s.tags.join(","),
                );
            }
            Ok(())
        }
        "sources" => run_skills_sources_subcommand().await,
        other => {
            // #366: Surface a "did you mean?" hint for typos like
            // `skills sourcs` -> `skills sources`.
            let known = &["list", "sources"];
            if let Some(s) = cli::did_you_mean(other, known, 2) {
                eprintln!("open-mpm skills: unknown subcommand '{other}'. Did you mean '{s}'?");
            } else {
                eprintln!(
                    "open-mpm skills: unknown subcommand '{other}'. Try: list [--tag <tag>] | sources"
                );
            }
            bail!("unknown skills subcommand: {other}");
        }
    }
}

/// Print configured skill sources (`open-mpm skills sources`) (#172).
///
/// Why: Operators editing `.open-mpm/skill-sources.toml` need to confirm which
/// sources the harness actually loaded, whether each is enabled, and how many
/// skills each contributed. Otherwise misconfiguration is silent.
/// What: Loads the source registry, scans each path, and prints a one-line
/// summary per source: priority, type, identifier, enabled flag, skill count.
/// Test: Smoke-tested by invoking `cargo run -- skills sources`; correctness
/// of the underlying machinery is covered by `SkillSourceRegistry` unit tests.
async fn run_skills_sources_subcommand() -> Result<()> {
    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let source_registry = skills::sources::SkillSourceRegistry::load(&project_root);
    let sources = source_registry.sources();
    let resolved = source_registry.resolved_paths();

    println!("Sources ({}):", sources.len());

    // Track the resolved-path index so we can map enabled sources to their
    // computed dir and count skills there.
    let mut resolved_iter = resolved.iter();
    for source in sources {
        let type_label = match source.source_type {
            skills::sources::SkillSourceType::Local => "local",
            skills::sources::SkillSourceType::RemoteGit => "remote",
        };
        let identifier = match source.source_type {
            skills::sources::SkillSourceType::Local => {
                source.path.clone().unwrap_or_else(|| "<unset>".to_string())
            }
            skills::sources::SkillSourceType::RemoteGit => source
                .name
                .clone()
                .or_else(|| source.url.clone())
                .unwrap_or_else(|| "<unnamed>".to_string()),
        };
        let enabled_label = if source.enabled {
            "enabled"
        } else {
            "disabled"
        };
        let approval_label = if source.approved { "" } else { " (unapproved)" };

        let skill_count = if source.enabled {
            // Pull the matching resolved path off the iterator and count `.md`
            // files there.
            resolved_iter
                .next()
                .map(|p| count_skill_files(p))
                .unwrap_or(0)
        } else {
            0
        };

        println!(
            "  [{prio:>2}] {type_label:<7} {ident:<32} {enabled_label} {count} skills{approval}",
            prio = source.priority,
            type_label = type_label,
            ident = identifier,
            enabled_label = enabled_label,
            count = skill_count,
            approval = approval_label,
        );
    }
    Ok(())
}

/// Count `.md` files reachable under `dir` recursively (zero when missing).
fn count_skill_files(dir: &Path) -> usize {
    if !dir.is_dir() {
        return 0;
    }
    let mut count = 0;
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            count += count_skill_files(&path);
        } else if path.extension().and_then(|s| s.to_str()) == Some("md") {
            count += 1;
        }
    }
    count
}

/// Label a skill source path against the known hierarchy (#168).
///
/// Why: Users care which layer produced a skill, not its absolute path. We
/// only match on the *prefix* (not parent equality like the agents helper)
/// because bundled skills live in nested subdirs (e.g.
/// `.open-mpm/skills/frameworks/fastapi.md`).
fn classify_skill_source(source: &Path, bundled: &Path, home: Option<&Path>) -> String {
    if source.starts_with(bundled) {
        return "bundled".to_string();
    }
    if source.starts_with(".open-mpm/skills") {
        return ".open-mpm/skills".to_string();
    }
    if source.starts_with(".claude/skills") {
        return ".claude/skills".to_string();
    }
    if let Some(home) = home {
        if source.starts_with(home.join(".open-mpm/skills")) {
            return "~/.open-mpm/skills".to_string();
        }
        if source.starts_with(home.join(".claude/skills")) {
            return "~/.claude/skills".to_string();
        }
    }
    source.display().to_string()
}

/// Turn an absolute source path into a short label for `agents list` output.
///
/// Why: Users care which layer of the search path an agent came from, not
/// the full absolute path. Mapping known dirs to labels keeps output tidy.
/// What: Returns `bundled`, `.open-mpm/agents`, `.claude/agents`,
/// `~/.open-mpm/agents`, `~/.claude/agents`, or the full path as fallback.
fn classify_source(source: &Path, bundled: &Path, home: Option<&Path>) -> String {
    let parent = source.parent();
    if let Some(parent) = parent {
        if parent == bundled {
            return "bundled".to_string();
        }
        if parent == Path::new(".open-mpm/agents") {
            return ".open-mpm/agents".to_string();
        }
        if parent == Path::new(".claude/agents") {
            return ".claude/agents".to_string();
        }
        if let Some(home) = home {
            if parent == home.join(".open-mpm/agents") {
                return "~/.open-mpm/agents".to_string();
            }
            if parent == home.join(".claude/agents") {
                return "~/.claude/agents".to_string();
            }
        }
    }
    source.display().to_string()
}
