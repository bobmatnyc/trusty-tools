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

//! Direct-agent mode and the AST-native bake-off comparison runner and report formatter.

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

/// Direct mode: bypass the PM LLM and call a sub-agent with raw task text.
///
/// Why: The PM orchestration layer adds an extra LLM hop that is unnecessary
/// when the caller already knows which agent should handle the task. Direct
/// mode is useful for bake-off challenges and scripting, and lets us iterate
/// on sub-agent prompts without burning PM tokens.
/// What: Reads task text from `--task-file` (or stdin if absent), spawns the
/// named sub-agent subprocess via `spawn_subagent_and_run`, prints the result
/// content to stdout, and optionally extracts `## File: <path>` sections into
/// `--out-dir`.
/// Test: `echo "Write a Python hello world" | trusty-agents --direct python-engineer`
/// should print a Python snippet.
pub(super) async fn run_direct(
    agent_name: &str,
    task_file: Option<&str>,
    inline_task: Option<&str>,
    out_dir: Option<&str>,
) -> Result<()> {
    let task = super::workflow_mode::read_task_text_with_inline(task_file, inline_task).await?;
    if task.is_empty() {
        bail!("empty task");
    }

    tracing::info!(agent = %agent_name, "direct mode: calling sub-agent");

    let result = spawn_subagent_and_run(agent_name, &task).await?;

    let content = match result {
        IpcMessage::Result { content, .. } => content,
        IpcMessage::Error { error, .. } => bail!("sub-agent error: {error}"),
        IpcMessage::Task { .. } => bail!("unexpected Task message from sub-agent"),
    };
    println!("{content}");

    if let Some(dir) = out_dir {
        // MIN-1 (#99): Use the shared `extract_files_from_content` in `ipc`
        // instead of a duplicate extractor.
        let base = std::path::Path::new(dir);
        let files = ipc::extract_files_from_content(&content);
        let count = files.len();
        for (filename, body) in files {
            let full_path = base.join(&filename);
            if let Some(parent) = full_path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .with_context(|| format!("failed to create dir: {}", parent.display()))?;
            }
            let mut final_body = body;
            if !final_body.ends_with('\n') {
                final_body.push('\n');
            }
            tokio::fs::write(&full_path, final_body.as_bytes())
                .await
                .with_context(|| format!("failed to write file: {}", full_path.display()))?;
            eprintln!("Wrote: {}", full_path.display());
        }
        if count == 0 {
            tracing::warn!(
                "no `## File: <path>` sections detected in output; nothing written to {dir}"
            );
        } else {
            tracing::info!(count = count, dir = %dir, "extracted files");
        }
    }

    Ok(())
}

/// Observable run statistics for a single bake-off invocation (#348).
///
/// Why: `run_compare_bakeoff` needs a uniform record per side so the report
/// table is symmetric. Token counts are tracked best-effort (zero when the
/// runner does not surface them).
/// What: Output file paths + their byte sizes, plus elapsed wall-clock.
/// Test: Implicit via the `--compare` smoke run.
#[derive(Debug, Default, Clone)]
struct BakeoffRunStats {
    elapsed_ms: u128,
    output_files: Vec<PathBuf>,
    total_bytes: usize,
    syntax_valid: Option<bool>,
}

/// #348: Run a task twice (traditional + AST-native) and emit a comparison
/// report.
///
/// Why: Bake-off operators want a one-shot way to see whether the AST
/// substrate produces equivalent (or better) output than the traditional
/// write-file path. Re-running the same task with both modes side-by-side
/// keeps the comparison apples-to-apples.
/// What: Resolves the agent name (defaulting to `engineer`), runs `run_direct`
/// twice with separate `out_dir`s — once with the override off, once on —
/// then walks each directory for produced files and prints a markdown table.
/// The report is also written to `out/compare-report-<ts>.md`.
/// Test: Manual smoke (see `--compare` instructions in CLAUDE.md).
pub(super) async fn run_compare_bakeoff(
    direct: Option<&str>,
    workflow: Option<&str>,
    task_file: Option<&str>,
    inline_task: Option<&str>,
) -> Result<()> {
    if direct.is_none() && workflow.is_none() {
        bail!("--compare requires --direct <agent> or --workflow <name>");
    }
    let task = super::workflow_mode::read_task_text_with_inline(task_file, inline_task).await?;
    if task.is_empty() {
        bail!("--compare requires --task or --task-file");
    }

    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let trad_dir = format!("out/compare-traditional-{ts}");
    let ast_dir = format!("out/compare-ast-{ts}");

    // Traditional run: ensure override is off.
    ast::set_ast_native_override(false);
    eprintln!("=== compare run 1 / 2 — traditional substrate (out: {trad_dir}) ===");
    let t0 = std::time::Instant::now();
    let trad_inline = task.clone();
    if let Some(name) = direct {
        run_direct(name, None, Some(trad_inline.as_str()), Some(&trad_dir)).await?;
    } else if let Some(name) = workflow {
        super::workflow_mode::run_workflow(
            name,
            None,
            Some(trad_inline.as_str()),
            Some(&trad_dir),
            None,
            false,
        )
        .await?;
    }
    let trad_stats = collect_run_stats(Path::new(&trad_dir), t0.elapsed().as_millis());

    // AST-native run.
    ast::set_ast_native_override(true);
    eprintln!("=== compare run 2 / 2 — AST-native substrate (out: {ast_dir}) ===");
    let t1 = std::time::Instant::now();
    let ast_inline = task.clone();
    if let Some(name) = direct {
        run_direct(name, None, Some(ast_inline.as_str()), Some(&ast_dir)).await?;
    } else if let Some(name) = workflow {
        super::workflow_mode::run_workflow(
            name,
            None,
            Some(ast_inline.as_str()),
            Some(&ast_dir),
            None,
            false,
        )
        .await?;
    }
    let ast_stats = collect_run_stats(Path::new(&ast_dir), t1.elapsed().as_millis());
    ast::set_ast_native_override(false);

    let report = format_compare_report(&task, &trad_stats, &ast_stats, &trad_dir, &ast_dir);
    println!("{report}");

    let report_path = format!("out/compare-report-{ts}.md");
    if let Some(parent) = Path::new(&report_path).parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    if let Err(e) = tokio::fs::write(&report_path, &report).await {
        tracing::warn!(error = %e, path = %report_path, "failed to write compare report");
    } else {
        eprintln!("Report written to {report_path}");
    }
    Ok(())
}

/// Walk `dir` collecting non-hidden file paths and their sizes for the
/// compare report.
fn collect_run_stats(dir: &Path, elapsed_ms: u128) -> BakeoffRunStats {
    let mut stats = BakeoffRunStats {
        elapsed_ms,
        ..Default::default()
    };
    if !dir.exists() {
        return stats;
    }
    for entry in walkdir::WalkDir::new(dir).follow_links(false) {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|n| n.starts_with('.'))
            .unwrap_or(false)
        {
            continue;
        }
        let size = entry.metadata().map(|m| m.len() as usize).unwrap_or(0);
        stats.total_bytes += size;
        stats.output_files.push(path.to_path_buf());
    }
    // Best-effort syntax check on Rust/Python/JS/Go outputs.
    let mut all_valid = true;
    let mut had_check = false;
    for f in &stats.output_files {
        if let Some((lang, _)) = ast::detect_language(f) {
            had_check = true;
            if let Ok(src) = std::fs::read_to_string(f)
                && ast::validate_syntax(&src, lang).is_err()
            {
                all_valid = false;
            }
        }
    }
    if had_check {
        stats.syntax_valid = Some(all_valid);
    }
    stats
}

/// Build a markdown comparison report from two `BakeoffRunStats`.
fn format_compare_report(
    task: &str,
    trad: &BakeoffRunStats,
    ast: &BakeoffRunStats,
    trad_dir: &str,
    ast_dir: &str,
) -> String {
    let task_summary: String = task.lines().next().unwrap_or("").chars().take(80).collect();
    let pct = |a: usize, b: usize| -> String {
        if a == 0 {
            "—".to_string()
        } else {
            let delta = b as f64 / a as f64 - 1.0;
            format!("{:+.1}%", delta * 100.0)
        }
    };
    let yn = |o: Option<bool>| match o {
        Some(true) => "YES",
        Some(false) => "NO",
        None => "—",
    };
    format!(
        "═══ Bake-off Comparison Report (#348) ═══\n\
         Task: {task_summary}\n\
         Timestamp: {ts}\n\
         \n\
         |                  | Traditional | AST-Native | Δ |\n\
         |------------------|-------------|------------|---|\n\
         | Output files     | {trad_files} | {ast_files} | {files_delta} |\n\
         | Output bytes     | {trad_bytes} | {ast_bytes} | {bytes_delta} |\n\
         | Wall-clock (ms)  | {trad_ms} | {ast_ms} | {ms_delta} |\n\
         | Syntax valid     | {trad_valid} | {ast_valid} | — |\n\
         \n\
         Traditional out_dir: {trad_dir}\n\
         AST-native  out_dir: {ast_dir}\n\
         \n\
         Note: LLM call / token counts are not yet plumbed end-to-end through \n\
         `run_direct`. The AST-native verdict is based on observable file \n\
         outputs and syntax validity. To compare per-call token use, attach \n\
         a perf collector (see `src/perf.rs`).\n",
        ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
        trad_files = trad.output_files.len(),
        ast_files = ast.output_files.len(),
        files_delta = pct(trad.output_files.len(), ast.output_files.len()),
        trad_bytes = trad.total_bytes,
        ast_bytes = ast.total_bytes,
        bytes_delta = pct(trad.total_bytes, ast.total_bytes),
        trad_ms = trad.elapsed_ms,
        ast_ms = ast.elapsed_ms,
        ms_delta = if trad.elapsed_ms > 0 {
            format!(
                "{:+.1}%",
                (ast.elapsed_ms as f64 / trad.elapsed_ms as f64 - 1.0) * 100.0
            )
        } else {
            "—".to_string()
        },
        trad_valid = yn(trad.syntax_valid),
        ast_valid = yn(ast.syntax_valid),
    )
}
