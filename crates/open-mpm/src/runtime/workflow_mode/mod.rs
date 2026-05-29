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

//! Workflow execution mode: runs a named multi-phase workflow plus the task-text loading helpers it shares.

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

// Module layout (see #366 split): the `run_workflow` orchestration body lives
// here; the self-contained helpers (runner selection, init-context build,
// modified-file scan, build-counter read) live in `helpers.rs`.
mod helpers;

use helpers::{
    build_init_context, build_runner_for_workflow, check_workflow_project_dirs,
    collect_modified_files, load_skill_registry, load_tag_skill_registry, load_user_memory_suffix,
    read_current_build_number, refresh_global_skills_cache, resolve_out_dir,
};

// Re-export the task-text readers so sibling runtime modules can keep calling
// `workflow_mode::read_task_text` / `read_task_text_with_inline`.
pub(crate) use helpers::{read_task_text, read_task_text_with_inline};

/// Workflow mode: load a prescriptive workflow and iterate its phases.
///
/// Why: For bake-off tasks, a fixed pipeline (research -> plan -> code ->
/// QA -> observe) produces more reliable results than dynamic PM delegation.
/// What: Reads task text from `--task-file` (or stdin), constructs a
/// `WorkflowEngine` wired to `SubprocessAgentRunner`, runs the named
/// workflow, handles code-phase file extraction, and prints the final
/// observe report.
/// Test: `open-mpm --workflow prescriptive --task-file t.md --out-dir /tmp/x`
/// loads `.open-mpm/workflows/prescriptive.json` and runs each phase.
pub(super) async fn run_workflow(
    name: &str,
    task_file: Option<&str>,
    inline_task: Option<&str>,
    out_dir: Option<&str>,
    project_dir: Option<&str>,
    json_output: bool,
) -> Result<()> {
    // #218: Pre-flight check — emit clear, actionable errors when the
    // project's `.open-mpm/{agents,workflows}/` directories are missing.
    check_workflow_project_dirs()?;

    let task = read_task_text_with_inline(task_file, inline_task).await?;
    if task.is_empty() {
        bail!("empty task");
    }

    // #410: Capture the user's project directory at invocation time, BEFORE
    // any directory derivation/canonicalization runs. This is the directory
    // the harness was launched from, which is the user's actual project
    // source tree by default. Used downstream to (a) seed the default for
    // `--project-dir` when the flag was omitted, and (b) populate
    // `OPEN_MPM_PROJECT_DIR` for every spawned agent.
    let invocation_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // Default `--project-dir` to the invocation CWD when the flag was not
    // supplied. Pre-#410 behavior left `project_dir` as `None`, which caused
    // agents to run with `CWD = out_dir` and break source-file lookup. With
    // this default, agents always see the user's project tree as their CWD,
    // and the artifacts dir (`--out-dir`) is used only for workflow output
    // files (assignments.json, workflow-report.md, etc.).
    let invocation_cwd_str = invocation_cwd.to_string_lossy().to_string();
    let project_dir: Option<&str> = match project_dir {
        Some(p) => Some(p),
        None => Some(invocation_cwd_str.as_str()),
    };

    // #222 / #410: Full separation of "artifacts dir" (`--out-dir`) and
    // "code dir" (`--project-dir`). After #410, `--project-dir` defaults to
    // the invocation CWD when omitted, so the typical case is now:
    //   - both set explicitly → out_dir = artifacts, code_dir = project
    //   - only --project-dir set → both = project (legacy #220 behavior)
    //   - only --out-dir set → out_dir = artifacts, code_dir = invocation
    //     CWD (#410: agents now see project source, not artifacts)
    //   - neither → out_dir = auto-generated, code_dir = invocation CWD
    let (artifacts_input, code_input): (Option<&str>, Option<&str>) = match (out_dir, project_dir) {
        (Some(o), Some(p)) => {
            tracing::info!(
                out_dir = %o,
                project_dir = %p,
                "#222: separated artifacts dir (--out-dir) and code dir (--project-dir)"
            );
            (Some(o), Some(p))
        }
        (None, Some(p)) => {
            // #220 legacy: --project-dir alone overrides both.
            (Some(p), Some(p))
        }
        (Some(o), None) => (Some(o), None),
        (None, None) => (None, None),
    };

    // Auto-generate out_dir when not specified. Naming convention:
    // `out/<label>-v<version>-<YYYYMMDD>-<HHMMSS>` where:
    //   - label = shortened task-file stem (level-2.txt → l2) or workflow name
    //   - version = Cargo package version with dots stripped (0.1.17 → v0117)
    //   - timestamp = UTC wall clock at run start
    let out_dir_buf: Option<PathBuf> = resolve_out_dir(artifacts_input, task_file, name);

    // #222: Resolve the code dir. When `--project-dir` was supplied, we use
    // that (independent of out_dir). Otherwise, fall back to out_dir so
    // generated code and artifacts share a single directory (legacy mode).
    let code_dir_buf: Option<PathBuf> = match code_input {
        Some(p) => Some(PathBuf::from(p)),
        None => out_dir_buf.clone(),
    };
    if let (Some(o), Some(c)) = (out_dir_buf.as_deref(), code_dir_buf.as_deref())
        && o != c
    {
        tracing::info!(
            out_dir = %o.display(),
            code_dir = %c.display(),
            "#222: artifacts dir and code dir are distinct"
        );
    }

    // #47: Read the already-incremented build counter without re-incrementing,
    // so perf records match the startup banner. Falls back to 0 on read failure.
    let build_num = read_current_build_number().await.unwrap_or(0);

    // #60: Build the runner. Scan the workflow's agents for any that opt
    // into `runner = "claude-code"`; if so, construct a ClaudeCodeAgentRunner,
    // verify auth at startup (fail fast with a clear message), and wrap
    // everything in the dispatcher. Otherwise use the subprocess runner
    // directly so we don't pay the path-lookup cost on every run.
    //
    // Capture the project root (CWD at startup, which is still the
    // project root here — `out_dir_buf` is only used as child CWD later)
    // and forward an absolute agents config dir to child processes via
    // `OPEN_MPM_CONFIG_DIR`. Without this, sub-agents spawned with
    // `current_dir(out_dir)` would try to load `.open-mpm/agents/<name>.toml`
    // relative to `out_dir` — which doesn't exist — and every phase fails
    // with "failed to load agent config for '<name>': No such file".
    // Mirrors the pattern in `ctrl::run_pm_task` (src/ctrl/mod.rs ~line 205).
    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let agents_config_dir = project_root.join(".open-mpm").join("agents");
    // #410: Forward the resolved project_dir (== code_dir when separated, or
    // the invocation CWD when only --out-dir was supplied) so every spawned
    // sub-agent sees `OPEN_MPM_PROJECT_DIR` and runs with CWD anchored at
    // the user's source tree rather than the artifacts directory.
    let subprocess_runner: Arc<dyn tools::AgentRunner> = Arc::new(
        SubprocessAgentRunner::new()
            .with_config_dir(Some(agents_config_dir))
            .with_out_dir(out_dir_buf.clone())
            .with_code_dir(code_dir_buf.clone())
            .with_project_dir(code_dir_buf.clone()),
    );
    let runner = build_runner_for_workflow(name, subprocess_runner.clone()).await?;
    let perf_dir: PathBuf = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("docs")
        .join("performance");

    // #69/#70/#72: context + memory management subsystem.
    // All three are optional/gracefully degrading — if OPENROUTER_API_KEY is
    // absent, the indexer drops turns with a debug log and the cleaner skips
    // any LLM-backed steps. We spawn them unconditionally so the wiring is
    // testable and the store dir is created eagerly.
    let store_dir = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".open-mpm")
        .join("state")
        .join("history");
    let api_key = std::env::var("OPENROUTER_API_KEY").unwrap_or_default();
    let indexer = context::HistoryIndexer::spawn(store_dir.clone(), api_key.clone());
    let cleaner = context::cleaner::MemoryCleaner::spawn(store_dir.clone(), api_key.clone(), 20);

    // #81/#115: Scan .open-mpm/skills/ plus global paths and share the merged
    // registry across the workflow so each phase gets the most relevant skill
    // bodies as a prompt prefix.
    let skills_dir = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".open-mpm")
        .join("skills");
    let cwd_for_skills = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    // #115: Refresh the global skills cache (fire-and-forget).
    refresh_global_skills_cache(&cwd_for_skills).await;

    // #115/#128: Load project-local skills merged with global discovery paths
    // plus claude-mpm `.claude/skills/` (project + user) content.
    let skill_registry = load_skill_registry(&cwd_for_skills).await;

    // #128: Discover claude-mpm agents for dynamic loading. This populates
    // diagnostics only; the actual fallback happens inside
    // `AgentConfig::by_name_async` when a TOML is not found.
    let _claude_mpm_agents = agents::claude_mpm_loader::discover_agents(&cwd_for_skills)
        .await
        .unwrap_or_default();
    tracing::info!(
        count = _claude_mpm_agents.len(),
        "discovered claude-mpm agents"
    );

    // #108/#109: Project self-initialization + memory seeding. Runs
    // once per project per day (marker TTL) and is otherwise a no-op. The
    // produced `InitContext` is injected as a prefix into every agent phase.
    let init_ctx = build_init_context().await;

    // INTENT: Construct a SkillsLoader so the engine auto-injects relevant
    // skill bodies (language + framework detection) into every phase's prompt.
    let skills_loader = Arc::new(skills::SkillsLoader::new(skills_dir.clone()));

    // #118: Open the user-scoped memory store and extract a prompt suffix.
    // Backed by an embedded redb + usearch store at ~/.open-mpm/memory/.
    // Injected at lower priority than project context so project-specific
    // knowledge always wins. Non-fatal on failure.
    let user_memory_suffix = load_user_memory_suffix().await;

    // #173: Tag-indexed skill registry for pre-plan automatic skill discovery.
    // This is independent of the legacy `skill_registry` (which is the older
    // relevance-scored search index used for per-phase auto-injection). The
    // tag registry mirrors the PM startup load so workflow runs see the same
    // bundled+local skills.
    let tag_skill_registry = load_tag_skill_registry();

    let mut engine = WorkflowEngine::new(runner, PathBuf::from(".open-mpm/workflows"))
        .with_build(build_num)
        .with_perf_dir(Some(perf_dir))
        .with_indexer(Some(indexer))
        .with_skill_registry(Some(skill_registry))
        .with_skills_loader(Some(skills_loader))
        .with_init_context(init_ctx)
        .with_user_memory(user_memory_suffix)
        .with_tag_skill_registry(Some(tag_skill_registry))
        .with_progress(Some(Arc::new(progress::ProgressReporter::new())));

    // #84: If the workflow JSON declares `ticket_management` with
    // `enabled=true`, attach a TicketManager so the engine creates and closes
    // a GitHub tracking issue around the run. We peek at the config file
    // before calling `engine.run` so we can keep the manager optional.
    {
        let wf_path = if name.ends_with(".json") || name.contains('/') {
            PathBuf::from(name)
        } else {
            PathBuf::from(".open-mpm/workflows").join(format!("{name}.json"))
        };
        if let Ok(def) = workflow::WorkflowDef::load(&wf_path)
            && let Some(tm_cfg) = def.ticket_management.clone()
            && tm_cfg.enabled
        {
            let tm = workflow::TicketManager::new(tm_cfg);
            engine = engine.with_ticket_manager(tm);
        }
    }

    let (ctx, perf_record) = engine
        .run_with_perf_and_dirs(name, task, out_dir_buf.clone(), code_dir_buf.clone())
        .await
        .context("workflow execution failed")?;

    // #72: Kick off a cleanup pass after each workflow run. Fire-and-forget.
    cleaner.trigger();

    // #171/#174: Persist updated effectiveness/usage to
    // ~/.open-mpm/skills/index.json so the next run's ranking benefits from
    // this run's signal. Prefer the structured `## Skill Ratings` block from
    // observe-agent when available; fall back to a coarse status-derived
    // signal otherwise. Non-fatal: a write failure here never masks a
    // successful workflow result.
    let observe_out_for_ratings = ctx.phase_outputs.get("observe").map(String::as_str);
    super::update_skill_usage_after_run(&perf_record, observe_out_for_ratings);

    // Record this run into the cross-project session log at
    // ~/.open-mpm/sessions/runs.jsonl so CTRL's `search_sessions` can grep
    // over history. Non-fatal: a write failure here never masks a successful
    // workflow result.
    {
        let project_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let observe_out = ctx
            .phase_outputs
            .get("observe")
            .cloned()
            .unwrap_or_default();
        let score = session_record::extract_score(&observe_out);
        let files_modified: Vec<String> = out_dir_buf
            .as_deref()
            .map(|dir| collect_modified_files(dir))
            .unwrap_or_default();
        let record = session_record::record_from_perf(
            &perf_record,
            &project_path,
            task_file,
            files_modified,
            score,
        );
        if let Err(e) = session_record::append_run_record(&record).await {
            tracing::warn!(error = %e, "failed to append session record");
        }

        // Also record a one-line summary interaction so InteractionLog
        // grep can answer "what did this run actually do?". Non-fatal:
        // failures here never mask a successful workflow result.
        let session_id = format!("build{}", perf_record.build);
        let summary = format!(
            "workflow={} status={} cost=${:.2} mins={} task={}",
            record.workflow, record.status, record.cost_usd, record.duration_mins, record.task,
        );
        let ilog = interaction_log::InteractionLog::new(&project_path, &session_id);
        if let Err(e) = ilog.append("pm", &summary, None).await {
            tracing::warn!(error = %e, "failed to append interaction summary");
        }

        // #186: If any mistakes were recorded for this session, fire off
        // the postmortem agent in the background so it doesn't block the
        // user-visible result. The OPEN_MPM_RUN_ID is the session id used
        // by the subprocess mistake recorder; we also try the build label
        // since interaction logs use that.
        let run_id = std::env::var("OPEN_MPM_RUN_ID").unwrap_or_default();
        let candidate_ids: Vec<String> = [run_id.clone(), session_id.clone()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect();
        let mut mistake_count = 0usize;
        let mut hit_id: Option<String> = None;
        for sid in &candidate_ids {
            if let Ok(records) = mistake_log::MistakeLog::read_session(&project_path, sid)
                && !records.is_empty()
            {
                mistake_count = records.len();
                hit_id = Some(sid.clone());
                break;
            }
        }
        if let (count, Some(sid)) = (mistake_count, hit_id)
            && count > 0
        {
            eprintln!("\n⚠  {count} mistakes logged — running postmortem analysis...");
            // Fire-and-forget: spawn the postmortem in the background so we
            // never delay the main workflow result.
            let project_root = project_path.clone();
            tokio::spawn(async move {
                if let Err(e) = super::postmortem::trigger_postmortem(&project_root, &sid).await {
                    tracing::warn!(error = %e, "postmortem agent dispatch failed");
                }
            });
        }
    }

    // #64: File extraction between phases is now handled INSIDE
    // `WorkflowEngine::run` for any phase with `produces_files: true`, so QA
    // can run against materialized files. This post-run extraction is kept as
    // a fallback for legacy workflow configs that do not yet set
    // `produces_files` on their code phase — re-running extraction is
    // idempotent (same bytes written to the same paths), so it is safe either
    // way. The `--direct` mode (which does not go through the engine) still
    // relies on `extract_files_to_dir` below for one-shot extraction.
    if let (Some(dir), Some(code_output)) = (out_dir_buf.as_deref(), ctx.phase_outputs.get("code"))
    {
        // MIN-1 (#99): Use the shared `extract_files_from_content` from `ipc`
        // instead of a duplicate `extract_files_to_dir`. Writing the files
        // here is the fallback path for legacy workflow configs whose code
        // phase does not set `produces_files: true`.
        let files = ipc::extract_files_from_content(code_output);
        for (filename, content) in files {
            let dest = dir.join(&filename);
            if let Some(parent) = dest.parent() {
                if let Err(e) = tokio::fs::create_dir_all(parent).await {
                    tracing::warn!(path = %dest.display(), error = %e, "fallback extract: mkdir failed (non-fatal)");
                    continue;
                }
            }
            if let Err(e) = tokio::fs::write(&dest, &content).await {
                tracing::warn!(path = %dest.display(), error = %e, "fallback extract: write failed (non-fatal)");
            }
        }
    }

    // Determine the narrative. Preserves the pre-#151 behavior: observe
    // phase output wins, falling back to the last phase's output.
    let narrative = ctx
        .phase_outputs
        .get("observe")
        .cloned()
        .or_else(|| ctx.phase_outputs.values().last().cloned())
        .unwrap_or_default();

    if json_output {
        // #151 Phase 1: emit a full `PmResponse` JSON envelope instead of
        // the narrative-only output. Default (no `--json`) preserves the
        // historical stdout contract.
        let response = api::builder::build_from_workflow(
            &ctx,
            Some(&perf_record),
            narrative,
            Some(name),
            Vec::new(),
        );
        println!("{}", serde_json::to_string_pretty(&response)?);
    } else if !narrative.is_empty() {
        println!("{narrative}");
    }

    Ok(())
}
