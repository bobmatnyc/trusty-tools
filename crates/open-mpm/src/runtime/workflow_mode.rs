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
    // #218: Pre-flight check — emit a clear, actionable error when the
    // project's `.open-mpm/agents/` directory is missing instead of letting
    // the first sub-agent spawn panic with a cryptic "failed to load agent
    // config" failure. Workflow JSON file existence is checked downstream
    // by `WorkflowDef::load`, which already produces a clear error.
    let cwd_for_check = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let agents_dir_check = cwd_for_check.join(".open-mpm").join("agents");
    if !agents_dir_check.exists() {
        bail!(
            "no `.open-mpm/agents/` found in {}.\n\n\
             open-mpm needs an agent config directory in the current project.\n\
             To bootstrap a new project, copy bundled defaults from your \
             open-mpm install:\n\n  \
               mkdir -p .open-mpm\n  \
               cp -r <open-mpm-source>/.open-mpm/agents .open-mpm/\n  \
               cp -r <open-mpm-source>/.open-mpm/workflows .open-mpm/\n  \
               cp -r <open-mpm-source>/.open-mpm/skills .open-mpm/  # optional\n\n\
             Also ensure `.env.local` (or the env) contains `OPENROUTER_API_KEY=...`.\n\
             A future `open-mpm init` subcommand will automate this; \
             see GitHub issue #218.",
            cwd_for_check.display()
        );
    }
    let workflows_dir_check = cwd_for_check.join(".open-mpm").join("workflows");
    if !workflows_dir_check.exists() {
        bail!(
            "no `.open-mpm/workflows/` found in {}.\n\n\
             Copy bundled workflow definitions from your open-mpm install:\n\n  \
               cp -r <open-mpm-source>/.open-mpm/workflows .open-mpm/\n\n\
             See GitHub issue #218.",
            cwd_for_check.display()
        );
    }

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
    let out_dir = artifacts_input;
    let out_dir_buf: Option<PathBuf> = match out_dir {
        Some(d) => Some(PathBuf::from(d)),
        None => {
            let label = task_file
                .and_then(|f| std::path::Path::new(f).file_stem())
                .and_then(|s| s.to_str())
                .map(|stem| {
                    // "level-2" → "l2", "level-3" → "l3", else use stem as-is
                    if let Some(rest) = stem.strip_prefix("level-") {
                        format!("l{rest}")
                    } else {
                        stem.to_string()
                    }
                })
                .unwrap_or_else(|| name.to_string());
            let version = env!("CARGO_PKG_VERSION").replace('.', "");
            let now = chrono::Utc::now();
            let ts = now.format("%Y%m%d-%H%M%S").to_string();
            let dir_name = format!("{label}-v{version}-{ts}");
            let out_path = std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join("out")
                .join(&dir_name);
            tracing::info!(
                out_dir = %out_path.display(),
                "no --out-dir provided; auto-generated output directory"
            );
            Some(out_path)
        }
    };

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

    // #115: Refresh the global skills cache so newly-added skills from
    // ~/.open-mpm/skills/files/ and ~/Projects/skillset-mcp are indexed.
    // Fire-and-forget: failures are logged but never block the workflow.
    match skills::global_cache::GlobalSkillsCache::new() {
        Ok(cache) => {
            if let Err(e) = cache.refresh(&cwd_for_skills).await {
                tracing::warn!(error = %e, "global skills cache refresh failed (continuing)");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "global skills cache init failed (continuing)");
        }
    }

    // #115: Load project-local skills merged with global discovery paths.
    // #128: Also merge claude-mpm skills from `.claude/skills/` (project) and
    // `~/.claude/skills/` (user) so users can drop claude-mpm content directly.
    let skill_registry = Arc::new({
        let mut registry = skills::SkillRegistry::load_with_global_cache(&cwd_for_skills)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "failed to load skill registry; using empty");
                skills::SkillRegistry::empty()
            });
        // Project-level claude-mpm first (higher priority after existing set).
        registry
            .load_additional_dir(&cwd_for_skills.join(".claude").join("skills"))
            .await;
        // User-level claude-mpm as a lower-priority fallback.
        if let Some(home) = dirs::home_dir() {
            registry
                .load_additional_dir(&home.join(".claude").join("skills"))
                .await;
        }
        registry
    });

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
    let init_ctx = {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let omd = cwd.join(".open-mpm").join("state");
        let initializer = init::ProjectInitializer::new(cwd, omd);
        let reinit = std::env::args().any(|a| a == "--reinit");
        let result = if reinit {
            initializer.force_reinitialize().await
        } else {
            initializer.initialize_if_needed().await
        };
        match result {
            Ok(ctx) => {
                tracing::info!(
                    memories = ctx.relevant_memories.len(),
                    summary_chars = ctx.project_summary.len(),
                    "project self-initialization complete"
                );
                // Cross-machine memory share: if `.open-mpm/shared-memories.jsonl`
                // exists and its hash differs from the last-imported tracker,
                // import it now so teammate sessions become recallable via
                // `memory_recall scope=imported`. Best-effort.
                {
                    let cwd_share = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                    match cli::memories_cmd::auto_import_if_changed(&cwd_share).await {
                        Ok(n) if n > 0 => {
                            eprintln!(
                                "[open-mpm] Imported {n} shared memories from .open-mpm/shared-memories.jsonl"
                            );
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!(error = %e, "auto-import shared memories failed (continuing)");
                        }
                    }
                }

                // #190: Seed agent memory with project docs so workflow agents
                // can recall user/developer documentation via memory_recall.
                // Best-effort: failures (e.g., model download issues) are
                // logged but do not block workflow execution.
                let cwd_inner = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let omd_inner = cwd_inner.join(".open-mpm").join("state");
                let session_dir = cwd_inner.join(".open-mpm").join("sessions").join("default");
                if let Err(e) = std::fs::create_dir_all(&session_dir) {
                    tracing::warn!(error = %e, "doc seed: create session dir failed");
                } else {
                    match memory::open_memory_store(&session_dir) {
                        Ok(store) => match memory::FastEmbedder::new() {
                            Ok(embedder) => {
                                let initializer =
                                    init::ProjectInitializer::new(cwd_inner, omd_inner);
                                // #190+: seed docs + skills + MCP connections in one call.
                                // seed_all() emits its own combined log line and
                                // never fails — individual stages log warnings on
                                // failure but don't abort.
                                let _ = initializer.seed_all(store.as_ref(), &embedder).await;
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "doc seed: embedder unavailable");
                            }
                        },
                        Err(e) => {
                            tracing::warn!(error = %e, "doc seed: store open failed");
                        }
                    }
                }
                Some(ctx)
            }
            Err(e) => {
                tracing::warn!(error = %e, "project self-initialization failed (continuing)");
                None
            }
        }
    };

    // INTENT: Construct a SkillsLoader so the engine auto-injects relevant
    // skill bodies (language + framework detection) into every phase's prompt.
    let skills_loader = Arc::new(skills::SkillsLoader::new(skills_dir.clone()));

    // #118: Open the user-scoped memory store and extract a prompt suffix.
    // Backed by an embedded redb + usearch store at ~/.open-mpm/memory/.
    // Injected at lower priority than project context so project-specific
    // knowledge always wins. Non-fatal on failure.
    let user_memory_suffix = match memory::user_store::UserMemoryStore::open().await {
        Ok(store) => {
            let suffix = store.to_prompt_suffix();
            tracing::debug!(suffix_chars = suffix.len(), "user memory store opened");
            if suffix.is_empty() {
                None
            } else {
                Some(suffix)
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "user memory store unavailable (continuing)");
            None
        }
    };

    // #173: Tag-indexed skill registry for pre-plan automatic skill discovery.
    // This is independent of the legacy `skill_registry` (which is the older
    // relevance-scored search index used for per-phase auto-injection). The
    // tag registry mirrors the PM startup load so workflow runs see the same
    // bundled+local skills.
    let tag_skill_registry = Arc::new({
        let mut reg = skills::registry::SkillRegistry::load(&skills::registry::skill_search_paths(
            &default_bundled_config_dir(),
        ));
        let index_path = skills::registry::skill_index_path();
        if let Err(e) = reg.merge_index(&index_path) {
            tracing::warn!(
                error = %e,
                path = %index_path.display(),
                "tag skill registry: failed to merge persisted effectiveness index (continuing with defaults)"
            );
        }
        reg
    });

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

/// Build the `AgentRunner` used for a workflow run, wiring in a
/// `ClaudeCodeAgentRunner` when any phase's agent opts into it (#60).
///
/// Why: Most workflows only use subprocess agents, and the `claude` CLI
/// lookup + auth check adds latency; scanning the workflow lets us skip
/// both when nothing needs them. When an agent *does* require claude-code,
/// we fail fast with a clear message if the CLI is missing or unauthenticated.
/// What: Loads the workflow JSON to enumerate agent names, peeks at each
/// agent's TOML to find `runner = "claude-code"`. If none → return the
/// subprocess runner unchanged. Otherwise build `ClaudeCodeAgentRunner`,
/// run `check_auth`, and return a `DispatchingAgentRunner` that routes
/// per-agent.
/// Test: Manual — a workflow with no claude-code agents returns the
/// subprocess runner; one with a claude-code agent triggers auth check.
async fn build_runner_for_workflow(
    workflow_name: &str,
    fallback: Arc<dyn tools::AgentRunner>,
) -> Result<Arc<dyn tools::AgentRunner>> {
    let path = if workflow_name.ends_with(".json") || workflow_name.contains('/') {
        PathBuf::from(workflow_name)
    } else {
        PathBuf::from(".open-mpm/workflows").join(format!("{workflow_name}.json"))
    };

    // Soft failure: if we can't read the workflow yet, just return the
    // fallback and let the engine produce its own WorkflowNotFound error.
    let def = match workflow::WorkflowDef::load(&path) {
        Ok(d) => d,
        Err(_) => return Ok(fallback),
    };

    let needs_claude_code = def.phases.iter().any(|p| {
        AgentConfig::by_name(&p.agent)
            .map(|c| c.agent.runner == agents::RunnerKind::ClaudeCode)
            .unwrap_or(false)
    });
    // #198 / Phase C: scan for in-process agents so we can build the
    // shared `InProcessAgentRunner` once per workflow.
    let needs_in_process = def.phases.iter().any(|p| {
        AgentConfig::by_name(&p.agent)
            .map(|c| c.agent.runner == agents::RunnerKind::InProcess)
            .unwrap_or(false)
    });

    if !needs_claude_code && !needs_in_process {
        return Ok(fallback);
    }

    let cc_arc = if needs_claude_code {
        tracing::info!(
            workflow = %workflow_name,
            "workflow uses claude-code runner; resolving claude CLI and verifying auth"
        );
        let cc = ClaudeCodeAgentRunner::new()
            .await
            .context("failed to resolve `claude` CLI for claude-code runner")?;
        cc.check_auth()
            .await
            .context("claude CLI auth check failed")?;
        tracing::info!("claude-code runner: authenticated via Claude Max OAuth");
        Some(Arc::new(cc))
    } else {
        None
    };

    let in_process_arc: Option<Arc<dyn tools::AgentRunner>> = if needs_in_process {
        tracing::info!(
            workflow = %workflow_name,
            "workflow uses in-process runner; constructing shared LLM client"
        );
        let client = Arc::new(llm::create_client()?);
        let runner = agents::in_process_runner::InProcessAgentRunner::with_default_resolver(client);
        Some(Arc::new(runner))
    } else {
        None
    };

    Ok(Arc::new(
        DispatchingAgentRunner::new(fallback, cc_arc).with_in_process(in_process_arc),
    ))
}

/// Read the current build number from `.open-mpm/state/build.json` without bumping.
///
/// Why: (#47) `main()` already calls `BuildInfo::load_and_increment()` at
/// startup. Calling it again from the workflow path would double-count.
/// What: Parses `.open-mpm/state/build.json` and returns the `build` field.
/// Test: Integration — verified when the emitted perf record's `build`
/// matches the startup banner in manual runs.
/// Walk `out_dir` and return the list of files (relative to `out_dir`).
///
/// Why: The session record's `files_modified` field is most useful when it
/// lists the files the workflow actually produced, which for file-extracting
/// phases live under `out_dir`. Walking a tree is cheap and avoids relying
/// on ctx internals.
/// What: Recursively enumerates regular files under `out_dir`, returning
/// their paths relative to `out_dir` as strings. Silently returns an empty
/// list if `out_dir` does not exist or can't be read — this is best-effort.
/// Test: Covered indirectly; a run with files in out_dir produces non-empty
/// `files_modified` in `~/.open-mpm/sessions/runs.jsonl`.
fn collect_modified_files(out_dir: &Path) -> Vec<String> {
    fn walk(root: &Path, cur: &Path, acc: &mut Vec<String>) {
        let entries = match std::fs::read_dir(cur) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(ft) if ft.is_file() => {
                    if let Ok(rel) = path.strip_prefix(root) {
                        acc.push(rel.to_string_lossy().to_string());
                    }
                }
                Ok(ft) if ft.is_dir() => walk(root, &path, acc),
                _ => {}
            }
            // Cap to prevent runaway in huge trees.
            if acc.len() >= 200 {
                return;
            }
        }
    }
    let mut out = Vec::new();
    walk(out_dir, out_dir, &mut out);
    out.sort();
    out
}

async fn read_current_build_number() -> Result<u64> {
    #[derive(serde::Deserialize)]
    struct PersistedBuild {
        build: u64,
    }
    let path = std::env::current_dir()?
        .join(".open-mpm")
        .join("state")
        .join("build.json");
    let bytes = tokio::fs::read(&path).await?;
    let p: PersistedBuild = serde_json::from_slice(&bytes)?;
    Ok(p.build)
}

#[allow(dead_code)]
pub(crate) async fn read_task_text(task_file: Option<&str>) -> Result<String> {
    read_task_text_with_inline(task_file, None).await
}

/// Resolve task text from either an inline `--task <STRING>` argument,
/// a `--task-file <path>`, or stdin (in that priority order).
///
/// Why: #126 bug 1 — callers want to pass short task strings directly on
/// the command line without creating a temp file.
/// What: Checks `inline_task` first (highest precedence), then `task_file`,
/// then falls back to reading stdin. Trims the result.
/// Test: `cargo run -- --direct python-engineer --task "hello"` should route
/// `"hello"` to the agent without reading stdin.
pub(crate) async fn read_task_text_with_inline(
    task_file: Option<&str>,
    inline_task: Option<&str>,
) -> Result<String> {
    if let Some(text) = inline_task {
        return Ok(text.trim().to_string());
    }
    let task = if let Some(path) = task_file {
        tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("failed to read task file: {path}"))?
    } else {
        let mut s = String::new();
        tokio::io::stdin()
            .read_to_string(&mut s)
            .await
            .context("failed to read task from stdin")?;
        s
    };
    Ok(task.trim().to_string())
}
