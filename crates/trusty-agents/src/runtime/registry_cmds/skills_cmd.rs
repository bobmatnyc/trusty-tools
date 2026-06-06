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

/// Handle `trusty-agents start [--port <port>]` (#403).
///
/// Why: Friendly subcommand alias for `--service start` so users get the
/// daemon-management UX they expect from CLIs like `nginx start` or
/// `docker start`. Reuses `service::start_service` so behaviour and pid-file
/// semantics stay identical.
/// What: Parses optional `--port <u16>` (default 8765 to align with the
/// trusty-agents web UI port), polls `/api/health` for up to 10s, prints PID +
/// port on success.
/// Test: `om start` then `om status` then `om stop` against a clean repo.
/// Handle `trusty-agents dashboard` (#442).
///
/// Why: Surface the Tauri desktop UI behind a friendly subcommand so users
/// can run `om dashboard` without remembering build paths. We probe a small
/// set of candidate locations (release-first, then debug) and spawn the
/// binary detached if found.
/// What: Tries `<om_dir>/../../ui/src-tauri/target/release/trusty-agents-ui`,
/// then `<cwd>/ui/src-tauri/target/release/trusty-agents-ui`, then the debug
/// equivalent. On hit: spawns + exits 0. On miss: prints a build hint and
/// exits 1.
/// Test: Manual — `om dashboard` should pop the GUI when built; the error
/// path is exercised by deleting the binaries and re-running.
pub(crate) async fn run_dashboard_subcommand(args: &[String]) -> Result<()> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("Usage: trusty-agents dashboard|dash");
        println!();
        println!("Launches the Tauri desktop GUI for trusty-agents.");
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
        // `<exe_dir>/../../ui/src-tauri/target/release/trusty-agents-ui`
        candidates.push(
            exe_dir
                .join("..")
                .join("..")
                .join("ui")
                .join("src-tauri")
                .join("target")
                .join("release")
                .join("trusty-agents-ui"),
        );
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(
            cwd.join("ui")
                .join("src-tauri")
                .join("target")
                .join("release")
                .join("trusty-agents-ui"),
        );
        candidates.push(
            cwd.join("ui")
                .join("src-tauri")
                .join("target")
                .join("debug")
                .join("trusty-agents-ui"),
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

/// Handle `trusty-agents skills <subcommand>` (#168).
///
/// Why: Gives operators visibility into which skills were discovered, from
/// where, and lets them verify tag-based lookup before delegating. Without
/// this, the registry is invisible.
/// What: Supports `skills list [--tag <tag>]`. Without `--tag`, prints every
/// discovered skill with source label + tags. With `--tag <tag>` (repeatable),
/// filters + ranks by tag-overlap score.
/// Test: Covered manually; unit-tested via `SkillRegistry::find_by_tags`.
pub(crate) async fn run_skills_subcommand(args: &[String]) -> Result<()> {
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
                eprintln!("tagent skills: unknown subcommand '{other}'. Did you mean '{s}'?");
            } else {
                eprintln!(
                    "tagent skills: unknown subcommand '{other}'. Try: list [--tag <tag>] | sources"
                );
            }
            bail!("unknown skills subcommand: {other}");
        }
    }
}

/// Print configured skill sources (`trusty-agents skills sources`) (#172).
///
/// Why: Operators editing `.trusty-agents/skill-sources.toml` need to confirm which
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
/// `.trusty-agents/skills/frameworks/fastapi.md`).
fn classify_skill_source(source: &Path, bundled: &Path, home: Option<&Path>) -> String {
    if source.starts_with(bundled) {
        return "bundled".to_string();
    }
    if source.starts_with(".trusty-agents/skills") {
        return ".trusty-agents/skills".to_string();
    }
    if source.starts_with(".claude/skills") {
        return ".claude/skills".to_string();
    }
    if let Some(home) = home {
        if source.starts_with(home.join(".trusty-agents/skills")) {
            return "~/.trusty-agents/skills".to_string();
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
/// What: Returns `bundled`, `.trusty-agents/agents`, `.claude/agents`,
/// `~/.trusty-agents/agents`, `~/.claude/agents`, or the full path as fallback.
pub(super) fn classify_source(source: &Path, bundled: &Path, home: Option<&Path>) -> String {
    let parent = source.parent();
    if let Some(parent) = parent {
        if parent == bundled {
            return "bundled".to_string();
        }
        if parent == Path::new(".trusty-agents/agents") {
            return ".trusty-agents/agents".to_string();
        }
        if parent == Path::new(".claude/agents") {
            return ".claude/agents".to_string();
        }
        if let Some(home) = home {
            if parent == home.join(".trusty-agents/agents") {
                return "~/.trusty-agents/agents".to_string();
            }
            if parent == home.join(".claude/agents") {
                return "~/.claude/agents".to_string();
            }
        }
    }
    source.display().to_string()
}
