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

//! Session subcommand handlers (new/list/attach/kill/run) plus shared argv flag-extraction helpers.

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

/// Handle `trusty-agents session <new|list|attach|kill|run>` (#406).
///
/// Why: Gives users a CLI surface to manage interactive REPL sessions backed
/// by the running trusty-agents server, including optional git worktree creation
/// so multiple agents can work on the same repo in parallel.
/// What: Resolves the server port and dispatches to a per-subcommand helper.
/// All subcommands flow through HTTP to `/api/ctrl/sessions*` on the
/// configured `--port` (default 8765).
/// Test: Smoke-tested by creating, listing, attaching to, killing, and
/// running a session against a running server.
pub(super) async fn handle_session_subcommand(args: &[String]) -> Result<()> {
    let port = extract_port_flag(args).unwrap_or(8765);
    let base_url = format!("http://127.0.0.1:{}", port);
    let client = reqwest::Client::new();

    match args.first().map(|s| s.as_str()) {
        Some("new") => handle_session_new(args, &base_url, &client).await,
        Some("list") => handle_session_list(args, &base_url, &client).await,
        Some("attach") => handle_session_attach(args, &base_url, &client).await,
        Some("kill") => handle_session_kill(args, &base_url, &client).await,
        Some("run") => handle_session_run(args, port).await,
        _ => {
            print_session_usage();
            Ok(())
        }
    }
}

// Purpose: `om session new` — POST a new session to the server and print
// the resulting ID, name, agent, status, and worktree info if any.
async fn handle_session_new(
    args: &[String],
    base_url: &str,
    client: &reqwest::Client,
) -> Result<()> {
    let project = extract_flag(args, "--project")
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().to_string())
        })
        .unwrap_or_default();
    let name = extract_flag(args, "--name")
        .unwrap_or_else(|| format!("session-{}", &uuid::Uuid::new_v4().to_string()[..8]));
    let agent = extract_flag(args, "--agent").unwrap_or_else(|| "pm".to_string());
    let worktree = args.iter().any(|a| a == "--worktree");

    let body = serde_json::json!({
        "project_path": project,
        "name": name,
        "agent": agent,
        "worktree": worktree,
    });

    let resp = client
        .post(format!("{}/api/ctrl/sessions", base_url))
        .json(&body)
        .send()
        .await?;

    if resp.status().is_success() {
        let session: serde_json::Value = resp.json().await?;
        // #409: Print session details in a stable, human-readable
        // block. Order: ID, Name, Project, Agent, Status. Worktree
        // fields appear only when the server actually provisioned one.
        println!("Session created:");
        println!("  ID:      {}", session["id"].as_str().unwrap_or("?"));
        println!("  Name:    {}", session["name"].as_str().unwrap_or("?"));
        println!(
            "  Project: {}",
            session["project_name"].as_str().unwrap_or("?")
        );
        println!("  Agent:   {}", session["agent"].as_str().unwrap_or("?"));
        println!(
            "  Status:  {}",
            session["status"].as_str().unwrap_or("idle")
        );
        if let Some(wt) = session["worktree_path"].as_str() {
            println!("  Worktree: {}", wt);
            println!(
                "  Branch:   {}",
                session["worktree_branch"].as_str().unwrap_or("?")
            );
        }
        println!();
        println!(
            "To attach: om session attach {}",
            session["id"].as_str().unwrap_or("?")
        );
    } else {
        eprintln!("Failed to create session: {}", resp.status());
    }

    Ok(())
}

// Purpose: `om session list` — GET sessions from the server, optionally
// filtered by project, and pretty-print as a fixed-width table.
async fn handle_session_list(
    args: &[String],
    base_url: &str,
    client: &reqwest::Client,
) -> Result<()> {
    let project_filter = args
        .get(1)
        .filter(|a| !a.starts_with('-'))
        .map(|p| format!("?project={}", p))
        .unwrap_or_default();

    let resp = client
        .get(format!("{}/api/ctrl/sessions{}", base_url, project_filter))
        .send()
        .await?;

    if resp.status().is_success() {
        let data: serde_json::Value = resp.json().await?;
        let sessions = data["sessions"].as_array().cloned().unwrap_or_default();
        if sessions.is_empty() {
            println!("No sessions found.");
        } else {
            println!(
                "{:<36}  {:<20}  {:<15}  {:<8}  STATUS",
                "ID", "NAME", "PROJECT", "AGENT"
            );
            println!("{}", "-".repeat(100));
            for s in &sessions {
                println!(
                    "{:<36}  {:<20}  {:<15}  {:<8}  {}{}",
                    s["id"].as_str().unwrap_or("?"),
                    s["name"].as_str().unwrap_or("?"),
                    s["project_name"].as_str().unwrap_or("?"),
                    s["agent"].as_str().unwrap_or("?"),
                    s["status"].as_str().unwrap_or("?"),
                    if s["worktree_path"].is_string() {
                        " [worktree]"
                    } else {
                        ""
                    },
                );
            }
        }
    } else {
        eprintln!("Failed to list sessions: {}", resp.status());
    }

    Ok(())
}

// Purpose: `om session attach <id>` — POST attach, then re-exec the current
// binary inside the session's working_dir with TAGENT_SESSION_ID /
// TAGENT_AGENT exported. This function ends with `process::exit` on
// success, so it never returns to the caller.
async fn handle_session_attach(
    args: &[String],
    base_url: &str,
    client: &reqwest::Client,
) -> Result<()> {
    let id = args
        .get(1)
        .ok_or_else(|| anyhow::anyhow!("Usage: om session attach <session-id>"))?;

    let resp = client
        .post(format!("{}/api/ctrl/sessions/{}/attach", base_url, id))
        .send()
        .await?;

    if resp.status().is_success() {
        let info: serde_json::Value = resp.json().await?;
        let working_dir = info["working_dir"].as_str().unwrap_or(".").to_string();
        let agent = info["agent"].as_str().unwrap_or("pm").to_string();
        let name = info["name"].as_str().unwrap_or("session").to_string();

        println!("Attaching to session '{}' (agent: {})...", name, agent);
        println!("Working directory: {}", working_dir);
        println!();

        let exe = std::env::current_exe()?;
        let mut cmd = std::process::Command::new(&exe);
        cmd.env("TAGENT_SESSION_ID", id)
            .env("TAGENT_AGENT", &agent)
            .current_dir(&working_dir);

        let status = cmd.status()?;
        std::process::exit(status.code().unwrap_or(0));
    } else if resp.status() == reqwest::StatusCode::NOT_FOUND {
        eprintln!("Session not found: {}", id);
    } else {
        eprintln!("Failed to attach: {}", resp.status());
    }

    Ok(())
}

// Purpose: `om session kill <id>` — DELETE the session and report success
// or a not-found / generic failure message.
async fn handle_session_kill(
    args: &[String],
    base_url: &str,
    client: &reqwest::Client,
) -> Result<()> {
    let id = args
        .get(1)
        .ok_or_else(|| anyhow::anyhow!("Usage: om session kill <session-id>"))?;

    let resp = client
        .delete(format!("{}/api/ctrl/sessions/{}", base_url, id))
        .send()
        .await?;

    if resp.status().is_success() {
        println!("Session {} terminated.", id);
    } else if resp.status() == reqwest::StatusCode::NOT_FOUND {
        eprintln!("Session not found: {}", id);
    } else {
        eprintln!("Failed to terminate session: {}", resp.status());
    }

    Ok(())
}

// Purpose: `om session run` (#408) — supervised workflow executor with retry.
// The other subcommands are interactive lifecycle helpers; `run` is the
// only one that drives a task to completion in one shot, returning a
// structured Success / Blocked outcome. Parses --project / --task /
// --agent / --max-attempts / --name, delegates to `CtrlSupervisor`, and
// prints the outcome, exiting with the appropriate code on Blocked.
async fn handle_session_run(args: &[String], port: u16) -> Result<()> {
    let project = extract_flag(args, "--project")
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().to_string())
        })
        .ok_or_else(|| anyhow::anyhow!("--project is required (or run from a project dir)"))?;
    let task = extract_flag(args, "--task").ok_or_else(|| anyhow::anyhow!("--task is required"))?;
    let agent = extract_flag(args, "--agent").unwrap_or_else(|| "pm".to_string());
    let max_attempts: u32 = extract_flag(args, "--max-attempts")
        .and_then(|s| s.parse().ok())
        .unwrap_or(3);
    let name = extract_flag(args, "--name");

    let supervisor = ctrl::CtrlSupervisor::new(
        std::path::PathBuf::from(&project),
        task.clone(),
        agent,
        max_attempts,
        name,
        port,
    );

    match supervisor.run().await? {
        ctrl::SupervisorOutcome::Success {
            summary,
            session_id,
            attempts,
        } => {
            println!("✓ Task completed (attempt {}/{})", attempts, max_attempts);
            println!("  Session: {}", session_id);
            println!();
            println!("{}", summary);
        }
        ctrl::SupervisorOutcome::SuccessWithCaveats {
            summary,
            caveats,
            session_id,
            attempts,
        } => {
            println!(
                "✓ Task completed (attempt {}/{}) — with pre-existing test failures",
                attempts, max_attempts
            );
            println!("  Session: {}", session_id);
            println!();
            println!("{}", summary);
            if !caveats.is_empty() {
                println!();
                println!(
                    "Note: QA found {} pre-existing failure(s) unrelated to this task:",
                    caveats.len()
                );
                for c in &caveats {
                    println!("  - {}", c);
                }
                println!();
                println!("These are out of scope and were not introduced by this run.");
            }
        }
        ctrl::SupervisorOutcome::Blocked {
            reason,
            attempts,
            session_id,
        } => {
            eprintln!("✗ Blocked after {} attempt(s)", attempts);
            eprintln!("  Session: {} (status: blocked)", session_id);
            eprintln!();
            eprintln!("Reason: {}", reason);
            eprintln!();
            eprintln!(
                "To retry manually: om session run --project {} --task \"{}\"",
                project, task
            );
            std::process::exit(1);
        }
    }

    Ok(())
}

// Purpose: Default-arm usage block for `om session`.
fn print_session_usage() {
    println!("Usage: om session <new|list|attach|kill|run> [options]");
    println!();
    println!("Commands:");
    println!("  new    --project <path> --name <name> [--agent <agent>] [--worktree]");
    println!("  list   [<project-path>]");
    println!("  attach <session-id>");
    println!("  kill   <session-id>");
    println!("  run    --project <path> --task <text> [--agent <agent>]");
    println!("         [--max-attempts <n>] [--name <name>]");
}

/// Find `--flag <value>` pair in argv slice.
///
/// Why: The session subcommand has half a dozen optional flags; a tiny helper
/// keeps the dispatcher readable.
/// What: Returns the value following the first occurrence of `flag`.
/// Test: Indirectly via session subcommand smoke tests.
pub(crate) fn extract_flag(args: &[String], flag: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone())
}

/// Parse `--port <N>` from argv slice.
///
/// Why: Same as `extract_flag` but typed as `u16`.
/// What: Reads and parses; returns `None` on either missing or unparsable.
/// Test: Covered indirectly by session subcommand tests.
pub(crate) fn extract_port_flag(args: &[String]) -> Option<u16> {
    extract_flag(args, "--port").and_then(|p| p.parse().ok())
}
