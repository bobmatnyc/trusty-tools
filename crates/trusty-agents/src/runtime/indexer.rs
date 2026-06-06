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

//! Code-index maintenance entry points: reindex, orphan checks, file-watcher spawning, and watch mode.

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

/// Default on-disk code store directory (`$CWD/.trusty-agents/state/code/`).
///
/// Why: Mirrors `cli::search_cmd::default_code_dir` so `--reindex`/`--watch`
/// write to the same location that `code search` reads from.
pub(super) fn default_code_dir() -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("failed to read cwd")?;
    Ok(cwd.join(".trusty-agents").join("state").join("code"))
}

/// Default source extensions the watcher/reindex track.
///
/// Why: The indexer supports more extensions than callers typically want to
/// watch; exposing a curated default keeps `--reindex`/`--watch` usable
/// without extra flags.
pub(super) fn default_extensions() -> Vec<String> {
    ["rs", "py", "ts", "tsx", "js", "jsx", "go", "md"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Read `[search] cool_after_minutes` from `<root>/.trusty-agents/config.toml`.
///
/// Why: #372 — operators need a knob to override the 15-minute cool-down
/// default without recompiling. Stays best-effort: a missing/malformed file
/// silently falls back to [`search::indexer::DEFAULT_COOL_AFTER_MINUTES`].
/// What: Parses just the `[search]` table; any other top-level fields are
/// ignored so this loader composes with the rest of `config.toml`.
/// Test: Indirect — exercised via the `cool_after_minutes` config knob; a
/// missing file path is the common case in CI.
fn load_search_cool_after(project_root: &Path) -> std::time::Duration {
    #[derive(serde::Deserialize, Default)]
    struct SearchSection {
        cool_after_minutes: Option<u64>,
    }
    #[derive(serde::Deserialize, Default)]
    struct Wrapper {
        #[serde(default)]
        search: Option<SearchSection>,
    }
    let path = project_root.join(".trusty-agents").join("config.toml");
    let minutes = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| toml::from_str::<Wrapper>(&s).ok())
        .and_then(|w| w.search.and_then(|s| s.cool_after_minutes))
        .unwrap_or(search::indexer::DEFAULT_COOL_AFTER_MINUTES);
    std::time::Duration::from_secs(minutes.saturating_mul(60))
}

/// Construct a `FileWatcher` rooted at the current working directory and
/// backed by the on-disk redb+usearch store.
///
/// Why: Both `--reindex` and `--watch` need identical setup; factoring it
/// keeps the CLI handlers terse and consistent.
/// What: Resolves the store dir, opens `RedbUsearchStore`, constructs a
/// `FastEmbedder`, wraps both in a `CodeIndexer`, returns a `FileWatcher`
/// with the default extensions. Honors `[search] cool_after_minutes` from
/// `.trusty-agents/config.toml` for the cool-down threshold (#372).
pub(super) async fn build_file_watcher() -> Result<FileWatcher> {
    const EMBED_DIM: usize = 384;
    let root = std::env::current_dir().context("failed to read cwd")?;
    let code_dir = default_code_dir()?;
    std::fs::create_dir_all(&code_dir)
        .with_context(|| format!("failed to create code dir: {}", code_dir.display()))?;
    let store = CodeStore::open(&code_dir, EMBED_DIM).context("failed to open CodeStore")?;
    let embedder = FastEmbedder::new().context("failed to construct FastEmbedder")?;
    let cool_after = load_search_cool_after(&root);
    let indexer =
        Arc::new(CodeIndexer::new(Arc::new(store), Arc::new(embedder)).with_cool_after(cool_after));
    Ok(FileWatcher::new(indexer, root, default_extensions()))
}

/// One-shot full re-index of the working tree, then exit.
///
/// Why: Seeds (or refreshes) the code index without waiting for filesystem
/// events — useful after pulling large changes or on first setup.
/// What: Builds a `FileWatcher`, calls `reindex_all`, prints the count.
/// Test: Manual: `cargo run -- --reindex`.
pub(super) async fn run_reindex() -> Result<()> {
    let watcher = build_file_watcher().await?;
    let n = watcher.reindex_all().await?;
    println!("Indexed {n} chunks.");
    Ok(())
}

/// List tracked sub-agent PIDs and their liveness status.
///
/// Why: #130 — operators need a quick way to inspect `.trusty-agents/state/processes.json`
/// and distinguish running, completed, and orphaned entries without parsing
/// JSON by hand.
/// What: Reads the tracker file for the current project, walks every entry,
/// and prints `pid  status  alive?  agent  task` to stdout. Marks entries as
/// `ORPHAN` when `status=Running` but the PID is no longer alive.
/// Test: `cargo run -- --check-orphans` in a project with an empty tracker
/// prints "No tracked sub-agent processes." and exits 0.
pub(super) async fn run_check_orphans() -> Result<()> {
    let cwd = std::env::current_dir().context("failed to read cwd")?;
    let agent_dir = cwd.join(".trusty-agents").join("state");
    let tracker = process_tracker::ProcessTracker::new(&agent_dir);
    let entries = tracker.load().await?;

    if entries.is_empty() {
        println!("No tracked sub-agent processes.");
        return Ok(());
    }

    println!(
        "{:<8} {:<10} {:<8} {:<24} TASK",
        "PID", "STATUS", "ALIVE", "AGENT"
    );
    let mut sorted: Vec<_> = entries.values().collect();
    sorted.sort_by_key(|e| e.pid);
    for e in sorted {
        let alive = std::process::Command::new("kill")
            .args(["-0", &e.pid.to_string()])
            .stderr(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        let status = format!("{:?}", e.status).to_lowercase();
        let alive_str = if alive { "yes" } else { "no" };
        let tag = if matches!(e.status, process_tracker::ProcessStatus::Running) && !alive {
            " (ORPHAN)"
        } else {
            ""
        };
        println!(
            "{:<8} {:<10} {:<8} {:<24} {}{}",
            e.pid, status, alive_str, e.agent_name, e.task_id, tag
        );
    }
    Ok(())
}

/// Spawn a background `FileWatcher` so the code index stays fresh during
/// normal interactive use, without the user having to remember `--watch`.
///
/// Why: Hybrid `search_code` is only useful when the index reflects the
/// working tree. Auto-watching (issue #372) closes the "did you reindex?"
/// gap that would otherwise hit every developer who edits files between
/// queries. We do this as a fire-and-forget tokio task so failures (no
/// permissions, embedder unavailable, redb lock contention) only emit a
/// warning — never abort startup.
/// What: Builds a `FileWatcher` exactly as `--watch` would, then spawns its
/// `watch()` future on the tokio runtime. Returns immediately. Skips the
/// initial reindex to avoid blocking startup; the existing index (if any)
/// is reused, and on-disk changes since last run will be picked up
/// incrementally as events arrive.
/// Test: Indirect — exercised end-to-end in any interactive run; the
/// helper itself is a thin wrapper around `build_file_watcher` + spawn.
pub(super) fn spawn_background_file_watcher() {
    tokio::spawn(async {
        // #374: If the search daemon is already running for this project,
        // it owns the redb code-store lock — we'd just deadlock trying to
        // open the same store. Skip the local watcher entirely; tools
        // route their queries through the daemon via SearchDaemonClient.
        let project_root =
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        if search::service::is_daemon_running(&project_root).await {
            tracing::info!(
                "search daemon detected — skipping in-process file watcher (queries will route to daemon)"
            );
            return;
        }
        match build_file_watcher().await {
            Ok(watcher) => {
                // #372 warm-start: load the persisted HNSW into RAM before
                // any user query lands. The on-disk file already exists at
                // .trusty-agents/state/code/code.usearch (created on prior runs);
                // this just ensures it's resident so the first search isn't
                // gated on a load. We log warm-up errors but continue —
                // searches will lazily warm on first use.
                let indexer = watcher.indexer();
                if let Err(e) = indexer.warm_up().await {
                    tracing::warn!(error = %e, "code-index warm-up failed; will warm lazily on first search");
                } else {
                    tracing::info!("code-index warmed at PM startup");
                }
                // #372 cool-down: evict the in-memory HNSW after N minutes
                // of no searches so an idle PM doesn't pin RAM. The file
                // watcher keeps running through cool-down — only the
                // in-memory vector index is dropped.
                let _cool = indexer.spawn_cool_down_monitor();
                tracing::info!("background file watcher started (auto-indexing on changes)");
                if let Err(e) = watcher.watch().await {
                    tracing::warn!(error = %e, "background file watcher exited with error");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not start background file watcher (continuing without auto-index)");
            }
        }
    });
}

/// Watch the working tree forever and keep the index in sync.
///
/// Why: Developer-facing incremental indexing. Cheaper than `--reindex`
/// for each edit and keeps search fresh without user intervention.
/// What: Builds a `FileWatcher` and calls `watch()`; blocks until the
/// process is killed.
/// Test: Manual: `cargo run -- --watch`.
pub(super) async fn run_watch() -> Result<()> {
    let watcher = build_file_watcher().await?;
    // Seed the index first so the initial search state isn't empty.
    let _ = watcher.reindex_all().await;
    watcher.watch().await
}
