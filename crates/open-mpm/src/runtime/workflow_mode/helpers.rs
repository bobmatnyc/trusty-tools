//! Standalone helpers for the workflow-execution mode.
//!
//! Why: `run_workflow` in `mod.rs` is a long orchestration body; pulling the
//! self-contained helper functions (runner selection, modified-file scan,
//! build-counter read) out keeps the entry-point file under the 500-line cap.
//! What: `build_runner_for_workflow`, `collect_modified_files`,
//! `read_current_build_number`.
//! Test: `build_runner_for_workflow` is documented as manually verified; the
//! others are exercised via the workflow integration path.

#![allow(dead_code)]

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::AsyncReadExt;

use crate::agents;
use crate::agents::AgentConfig;
use crate::agents::claude_code_runner::{ClaudeCodeAgentRunner, DispatchingAgentRunner};
use crate::build_info::BuildInfo;
use crate::{llm, tools, workflow};

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
pub(super) async fn build_runner_for_workflow(
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
pub(super) fn collect_modified_files(out_dir: &Path) -> Vec<String> {
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

pub(super) async fn read_current_build_number() -> Result<u64> {
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

/// Run project self-initialization + memory seeding, returning the injected
/// `InitContext` (or `None` when init fails).
///
/// Why (#108/#109): Extracted from `run_workflow` so the orchestration body
/// stays focused; the logic (marker-gated re-init, shared-memory auto-import,
/// doc/skill/MCP seeding) is unchanged.
/// What: Runs `ProjectInitializer::{initialize_if_needed,force_reinitialize}`,
/// auto-imports shared memories, seeds the agent store, and returns the ctx.
/// Test: Exercised end-to-end via the workflow integration tests.
pub(super) async fn build_init_context() -> Option<crate::init::InitContext> {
    use crate::{cli, init, memory};
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
                            let initializer = init::ProjectInitializer::new(cwd_inner, omd_inner);
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
}

/// Pre-flight check that the project's `.open-mpm/{agents,workflows}/` dirs
/// exist, emitting actionable bootstrap errors when they don't (#218).
///
/// Why: Extracted from `run_workflow` so the orchestration body stays focused;
/// failing here with a clear message beats a cryptic sub-agent spawn panic.
/// What: Bails with copy-paste bootstrap instructions when either dir is absent.
/// Test: Exercised via the workflow integration tests (missing-dir path).
pub(super) fn check_workflow_project_dirs() -> Result<()> {
    use anyhow::bail;
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
    Ok(())
}

/// Resolve the workflow artifacts directory, auto-generating one when the
/// caller omitted `--out-dir`.
///
/// Why: Extracted from `run_workflow` to keep the orchestration body focused.
/// What: Returns `Some(PathBuf::from(d))` when `out_dir` is set; otherwise
/// builds `./out/<label>-v<version>-<YYYYMMDD>-<HHMMSS>` from the task-file
/// stem (or workflow `name`) and logs the generated path.
/// Test: Exercised via the workflow integration tests.
pub(super) fn resolve_out_dir(
    out_dir: Option<&str>,
    task_file: Option<&str>,
    name: &str,
) -> Option<PathBuf> {
    match out_dir {
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
    }
}

/// Load the per-phase skill registry (project-local + global cache + claude-mpm
/// `.claude/skills/`).
///
/// Why (#115/#128): Extracted from `run_workflow` so the orchestration body
/// stays focused; the merge order (project-local, then project `.claude`, then
/// user `~/.claude`) is unchanged.
/// What: Returns an `Arc<SkillRegistry>` ready to hand to the engine; load
/// failures degrade to an empty registry with a WARN.
/// Test: Exercised via the workflow integration tests.
pub(super) async fn load_skill_registry(
    cwd_for_skills: &Path,
) -> Arc<crate::skills::SkillRegistry> {
    use crate::skills;
    Arc::new({
        let mut registry = skills::SkillRegistry::load_with_global_cache(cwd_for_skills)
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
    })
}

/// Build the tag-indexed skill registry used for pre-plan skill discovery (#173).
///
/// Why: Extracted from `run_workflow`; mirrors the PM startup load so workflow
/// runs see the same bundled + local skills with the persisted effectiveness
/// index merged in.
/// What: Loads `SkillRegistry` from the bundled search paths, merges the
/// effectiveness index (WARN-and-continue on failure), returns it in an `Arc`.
/// Test: Exercised via the workflow integration tests.
pub(super) fn load_tag_skill_registry() -> Arc<crate::skills::registry::SkillRegistry> {
    use crate::default_bundled_config_dir;
    use crate::skills;
    Arc::new(skills::registry::SkillRegistry::load_with_index(
        &default_bundled_config_dir(),
    ))
}

/// Open the user-scoped memory store and return its prompt suffix (#118).
///
/// Why: Extracted from `run_workflow`; the suffix is injected at lower
/// priority than project context so project-specific knowledge always wins.
/// What: Returns `Some(suffix)` when the store opens and has content, else
/// `None` (store-open failure is non-fatal and logged).
/// Test: Exercised via the workflow integration tests.
pub(super) async fn load_user_memory_suffix() -> Option<String> {
    use crate::memory;
    match memory::user_store::UserMemoryStore::open().await {
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
    }
}

/// Refresh the global skills cache so newly-added skills are indexed (#115).
///
/// Why: Extracted from `run_workflow`; fire-and-forget so cache failures never
/// block the workflow.
/// What: Constructs `GlobalSkillsCache` and refreshes it against `cwd`, logging
/// (but swallowing) any error.
/// Test: Exercised via the workflow integration tests.
pub(super) async fn refresh_global_skills_cache(cwd_for_skills: &Path) {
    crate::skills::global_cache::refresh_global_cache(cwd_for_skills).await;
}
