//! `WorkflowEngine` — iterates phases, runs each agent, threads outputs.
//!
//! Why: The prescriptive Research -> Plan -> Code -> QA -> Observe flow
//! needs a deterministic driver that feeds each phase's output into the
//! next via templates. An explicit engine keeps that orchestration logic
//! out of `main.rs` and behind a testable seam (`AgentRunner`).
//! What: `WorkflowEngine` holds an `Arc<dyn AgentRunner>` and a config dir;
//! `run(name)` loads the JSON, iterates phases, records outputs, writes
//! `workflow-report.md` to `out_dir` after the `observe` phase (if present).
//! Test: With a mock `AgentRunner` that returns fixed outputs per agent,
//! `run()` should produce a populated context and invoke the runner once
//! per phase. (This is exercised via integration; unit tests cover the
//! sub-pieces: config parsing, context templating.)

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use tracing::info;

use crate::agents::AgentConfig;
use crate::agents::persona::{detect_persona_matched, strip_persona_tag};
use crate::context::{HistoryIndexer, TurnRecord};
use crate::init::InitContext;
use crate::inspection::task_signals::TaskSignals;
use crate::ipc::{extract_files_from_content, extract_summary};
use crate::perf::PerfCollector;
use crate::session::SessionManager;
use crate::skills::registry::SkillRegistry as TagSkillRegistry;
use crate::skills::{SkillRegistry, SkillsLoader};
use crate::tools::traits::{AgentOutput, AgentRunner, RunContext};
use crate::workflow::autopush;
use crate::workflow::config::{Assignments, WorkflowDef};
use crate::workflow::context::WorkflowContext;
use crate::workflow::error::WorkflowError;
use crate::workflow::parallel::run_parallel_phase;
use crate::workflow::resolver::ConflictResolver;
use crate::workflow::tickets::TicketManager;

use super::helpers::{
    agent_uses_claude_code, reconcile_code_outputs_against, relocate_plan_outputs_from_project_root,
};
use super::qa::{QaStatus, parse_qa_envelope};
use super::skills::skill_summary_for;
use super::state::{emit_progress_event, phases_to_skip};
use super::step_dispatch::{discover_project_dir, run_wave_loop};

/// Workflow execution engine.
pub struct WorkflowEngine {
    agent_runner: Arc<dyn AgentRunner>,
    config_dir: PathBuf,
    /// #51: persistent agent sessions. When an agent's TOML sets
    /// `persistent_session = true`, the engine prepends this manager's
    /// history into each call and records the new exchange on success.
    sessions: SessionManager,
    /// #47: current build number stamped into perf records. Caller injects
    /// it via `with_build` — we avoid re-calling `BuildInfo::load_and_increment`
    /// here because that would double-bump the counter.
    build: u64,
    /// #47: root directory for performance output (typically
    /// `<cwd>/docs/performance`). When `None`, perf collection still runs
    /// in memory but is not flushed to disk — useful for tests.
    perf_dir: Option<PathBuf>,
    /// #70: optional background history indexer. When present, every agent
    /// turn's prompt+response is forwarded for embedding + persistence.
    indexer: Option<HistoryIndexer>,
    /// #81: optional skill registry used for per-phase auto-injection. When
    /// present AND non-empty, the top 2 relevant skill bodies are prepended
    /// to each phase's rendered task before the agent is invoked.
    skill_registry: Option<Arc<SkillRegistry>>,
    /// #81: cap on how many skills `auto_inject` pulls per phase.
    max_auto_skills: usize,
    /// #84: optional GitHub issue lifecycle manager. Wrapped in a
    /// `tokio::sync::Mutex` because `on_workflow_start` mutates the stored
    /// `issue_number` and `WorkflowEngine::run` takes `&self`.
    ticket_manager: Option<tokio::sync::Mutex<TicketManager>>,
    /// #108/#109: project self-initialization context. When present, its
    /// `to_prompt_prefix` output is prepended to every phase's rendered
    /// template so all agents share the project summary + memory seed.
    init_context: Option<InitContext>,
    /// Skills-first loader. When present, detects and injects language/framework
    /// skills into each phase's prompt before the legacy skill_registry path runs.
    /// Purely additive — absent means behavior is identical to before.
    skills_loader: Option<Arc<SkillsLoader>>,
    /// #118: User-scoped memory prompt suffix sourced from `~/.open-mpm/memory/`.
    /// Injected at the end of every phase's rendered task at lower priority than
    /// project context, so project-specific knowledge always takes precedence.
    /// `None` (or empty string) means no injection — effectively a no-op.
    user_memory_suffix: Option<String>,
    /// #173: Tag-indexed skill registry used for pre-plan automatic skill
    /// discovery. When present, the engine derives `TaskSignals` from the
    /// task text before the `plan` phase runs and injects a `## Available
    /// Skills` summary block into the plan-agent's prompt — removing the
    /// need for the plan-agent to call `list_skills` itself.
    tag_skill_registry: Option<Arc<TagSkillRegistry>>,
    /// #149: Real-time progress reporter that streams phase start/done lines
    /// to stderr while the workflow runs. When `None`, the engine runs
    /// silently (matches pre-#149 behavior). The reporter is shared via
    /// `Arc` so the wave loop can also emit per-wave events.
    progress: Option<Arc<crate::progress::ProgressReporter>>,
}

/// One skill surfaced by pre-plan discovery (#173).
///
/// Why: The engine returns a structured triple (name, summary, tags) instead
/// of full file contents so the plan-agent's prompt only carries a short
/// description per candidate skill — keeping the prompt tax bounded while
/// still letting the planner judge relevance for itself.
/// What: `name` is the canonical skill key; `summary` is either the
/// frontmatter `description` (preferred) or the first ~200 chars of the
/// stripped body; `tags` is the skill's tag list (informational).
/// Test: `discover_skills_for_task_extracts_python_fastapi_tags`,
/// `discover_skills_for_task_returns_top_n_by_effectiveness`.
#[derive(Debug, Clone)]
pub struct DiscoveredSkill {
    pub name: String,
    pub summary: String,
    pub tags: Vec<String>,
}

impl WorkflowEngine {
    /// Construct with an injected runner and the directory where workflow
    /// JSON files live (typically `config/workflows`).
    pub fn new(agent_runner: Arc<dyn AgentRunner>, config_dir: PathBuf) -> Self {
        Self {
            agent_runner,
            config_dir,
            sessions: SessionManager::new(),
            build: 0,
            perf_dir: None,
            indexer: None,
            skill_registry: None,
            max_auto_skills: 2,
            ticket_manager: None,
            init_context: None,
            skills_loader: None,
            user_memory_suffix: None,
            tag_skill_registry: None,
            progress: None,
        }
    }

    /// Attach a progress reporter (#149).
    ///
    /// Why: Streaming phase start/done events to stderr gives operators live
    /// feedback during 20–70 minute runs. Threading the reporter through the
    /// engine instead of constructing one per call keeps stderr output a
    /// caller-controlled concern (CLI yes, library tests no).
    /// What: Stores `Option<Arc<ProgressReporter>>`. `None` (the default)
    /// preserves silent behavior for tests and library consumers.
    /// Test: Engine integration via `main::run_workflow`; reporter formatting
    /// covered in `progress::tests`.
    pub fn with_progress(
        mut self,
        reporter: Option<Arc<crate::progress::ProgressReporter>>,
    ) -> Self {
        self.progress = reporter;
        self
    }

    /// Attach a tag-indexed skill registry for pre-plan auto-discovery (#173).
    ///
    /// Why: The plan-agent used to call `list_skills` manually to find
    /// relevant dependency-compatibility skills before writing
    /// `assignments.json`. That manual step burned a tool turn and was
    /// frequently skipped. Threading the tag-indexed registry into the engine
    /// lets us run discovery automatically before the plan phase and inject
    /// matched skills as a prompt prefix.
    /// What: Stores `Option<Arc<TagSkillRegistry>>`. A `None` (or empty
    /// registry) disables discovery — preserving exact prior behavior.
    /// Test: `discover_skills_for_task_extracts_python_fastapi_tags`.
    #[allow(dead_code)]
    pub fn with_tag_skill_registry(mut self, registry: Option<Arc<TagSkillRegistry>>) -> Self {
        self.tag_skill_registry = registry;
        self
    }

    /// Run pre-plan skill discovery against `task` text (#173).
    ///
    /// Why: The plan-agent benefits from seeing a curated list of available
    /// skills before it writes `assignments.json` so it can apply known
    /// version pins / compatibility rules without an extra `list_skills`
    /// tool call. Centralizing discovery in the engine means the same logic
    /// runs deterministically on every workflow invocation.
    /// What: Extracts language/framework/role/tag signals via
    /// `TaskSignals::extract`, queries the tag-indexed registry for the top
    /// `limit` matching skills, and returns each as a `DiscoveredSkill` with
    /// a short summary derived from the frontmatter description (or, when
    /// the description is empty, the first ~200 chars of the stripped body).
    /// Returns an empty `Vec` when the registry is absent / empty or no
    /// signals extract — never panics, never `unwrap()`s.
    /// Test: `discover_skills_for_task_extracts_python_fastapi_tags`,
    /// `discover_skills_for_task_returns_top_n_by_effectiveness`.
    pub fn discover_skills_for_task(&self, task: &str, limit: usize) -> Vec<DiscoveredSkill> {
        let Some(reg) = self.tag_skill_registry.as_ref() else {
            return Vec::new();
        };
        if reg.is_empty() {
            return Vec::new();
        }

        let signals = TaskSignals::extract(task);
        // Build a unioned tag set: explicit tags + languages + frameworks.
        // We intentionally do NOT include the role here — roles route agents,
        // not skills, and including them produces noisier matches.
        let mut tag_strings: Vec<String> = Vec::new();
        tag_strings.extend(signals.tags.iter().cloned());
        tag_strings.extend(signals.languages.iter().cloned());
        tag_strings.extend(signals.frameworks.iter().cloned());

        if tag_strings.is_empty() {
            tracing::debug!("skill discovery: no tags extracted from task; skipping");
            return Vec::new();
        }

        let tag_refs: Vec<&str> = tag_strings.iter().map(String::as_str).collect();
        let matches = reg.find_by_tags(&tag_refs);
        let names: Vec<String> = matches.iter().map(|m| m.name.clone()).collect();
        tracing::info!(
            count = names.len(),
            tags = ?tag_strings,
            matched = ?names,
            "skill discovery: {} skills matched for tags {:?}",
            names.len(),
            tag_strings,
        );

        matches
            .into_iter()
            .take(limit)
            .map(|meta| DiscoveredSkill {
                name: meta.name.clone(),
                summary: skill_summary_for(meta),
                tags: meta.tags.clone(),
            })
            .collect()
    }

    /// Attach a user-scoped memory prompt suffix (#118).
    ///
    /// Why: User memory is injected after all project-specific context so it
    /// enriches prompts with cross-project learnings without displacing the
    /// higher-priority project context.
    /// What: Stores `Option<String>`; a `None` or empty string is a no-op.
    /// Test: Pass a suffix containing "## User Memory", assert it appears at
    /// the end of the rendered phase task sent to the runner.
    #[allow(dead_code)]
    pub fn with_user_memory(mut self, suffix: Option<String>) -> Self {
        self.user_memory_suffix = suffix;
        self
    }

    /// Attach a project self-initialization context (#108/#109).
    ///
    /// Why: The CLI runs `ProjectInitializer::initialize_if_needed` once at
    /// workflow start; the engine needs the resulting context so every phase
    /// gets the project summary as a prompt prefix.
    /// What: Stores `Option<InitContext>`. When `Some`, each phase's rendered
    /// template is prepended with `init_ctx.to_prompt_prefix()`.
    /// Test: `init_context_is_prepended_to_phase_template` below.
    #[allow(dead_code)]
    pub fn with_init_context(mut self, ctx: Option<InitContext>) -> Self {
        self.init_context = ctx;
        self
    }

    /// Attach a `SkillsLoader` for skills-first prompt injection.
    ///
    /// Why: Replaces per-agent TOML skills lists with automatic language/framework
    /// detection so a single engineer.toml works across all stacks.
    /// What: Stores the loader; a `None` disables skills-first injection, falling
    /// back to the legacy `skill_registry` path unchanged.
    /// Test: Covered via `test_skills_loader_explicit_skills_loaded_correctly`.
    #[allow(dead_code)]
    pub fn with_skills_loader(mut self, loader: Option<Arc<SkillsLoader>>) -> Self {
        self.skills_loader = loader;
        self
    }

    /// Attach a `TicketManager` for automatic GitHub issue lifecycle (#84).
    ///
    /// Why: Lets workflow runs create and update a tracking issue without
    /// callers having to thread the manager through each phase. The manager
    /// short-circuits internally when `enabled=false`, so passing one whose
    /// config is disabled is effectively a no-op.
    /// What: Stores the manager behind a `tokio::sync::Mutex` so
    /// `on_workflow_start` (which needs `&mut self`) can be called from the
    /// `&self` phase loop. `None` disables the feature entirely.
    /// Test: `ticket_manager_noop_when_config_disabled` in `tickets.rs`
    /// exercises the disabled path; end-to-end requires a real repo.
    #[allow(dead_code)]
    pub fn with_ticket_manager(mut self, tm: TicketManager) -> Self {
        self.ticket_manager = Some(tokio::sync::Mutex::new(tm));
        self
    }

    /// Attach a skill registry for per-phase auto-injection (#81).
    ///
    /// Why: Lets workflow runs prepend the most relevant Markdown skill bodies
    /// to each phase's task so agents have domain knowledge in context without
    /// having to call `load_skill` explicitly.
    /// What: Stores the registry; a `None` (or empty registry) disables injection.
    /// Test: `auto_injects_skill_content_into_phase_task`.
    #[allow(dead_code)]
    pub fn with_skill_registry(mut self, registry: Option<Arc<SkillRegistry>>) -> Self {
        self.skill_registry = registry;
        self
    }

    /// Override the maximum number of skills auto-injected per phase (#81).
    #[allow(dead_code)]
    pub fn with_max_auto_skills(mut self, n: usize) -> Self {
        self.max_auto_skills = n;
        self
    }

    /// Attach a `HistoryIndexer` so each phase's turn is recorded (#70).
    #[allow(dead_code)]
    pub fn with_indexer(mut self, indexer: Option<HistoryIndexer>) -> Self {
        self.indexer = indexer;
        self
    }

    /// Stamp the engine with the current build counter for perf records (#47).
    #[allow(dead_code)]
    pub fn with_build(mut self, build: u64) -> Self {
        self.build = build;
        self
    }

    /// Set the perf output directory (typically `<cwd>/docs/performance`).
    ///
    /// Why: (#47) Tests leave this unset to skip the disk write; the CLI
    /// wires it to `docs/performance` so every `--workflow` run produces a
    /// run file + log line.
    /// What: Builder setter; `None` means do not flush.
    /// Test: Integration — `run_workflow` passes it through.
    #[allow(dead_code)]
    pub fn with_perf_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.perf_dir = dir;
        self
    }

    /// Inject a pre-built `SessionManager`.
    ///
    /// Why: Callers that want `--clear-sessions` semantics or that share a
    /// session store across multiple engine invocations should construct the
    /// manager themselves.
    /// What: Returns `self` with the provided manager installed (builder style).
    /// Test: Covered indirectly via `run_persistent_session_threads_history`.
    #[allow(dead_code)]
    pub fn with_sessions(mut self, sessions: SessionManager) -> Self {
        self.sessions = sessions;
        self
    }

    /// Load and run a named workflow.
    ///
    /// Why: Single entry point so the CLI only wires `--workflow <name>` to
    /// this function.
    /// What: Reads `{config_dir}/{name}.json`, initializes a
    /// `WorkflowContext`, iterates phases, calling the runner for each,
    /// writing outputs back to the context for later phase templates.
    /// After the loop, writes `workflow-report.md` into `out_dir` if set.
    /// Test: Integration-level; sub-pieces (config parse, templating) are
    /// unit-tested.
    pub async fn run(
        &self,
        name: &str,
        task: String,
        out_dir: Option<PathBuf>,
    ) -> Result<WorkflowContext, WorkflowError> {
        self.run_with_perf(name, task, out_dir)
            .await
            .map(|(ctx, _perf)| ctx)
    }

    /// Run the workflow with separate paths for artifacts (`out_dir`) and
    /// generated source code (`code_dir`).
    ///
    /// Why (#222): When a user runs `open-mpm --project-dir <existing-project>`
    /// they want generated source files to land in their project tree, while
    /// workflow artifacts (`assignments.json`, `workflow-report.md`, perf
    /// records, stubs) still live under `out_dir`. Threading two paths keeps
    /// artifacts colocated for inspection without polluting the user's project.
    /// What: Identical to `run_with_perf` but accepts a separate `code_dir`.
    /// When `code_dir` is `None`, falls back to `out_dir` (legacy behavior —
    /// generated code lands alongside artifacts).
    /// Test: `--project-dir . --out-dir /tmp/x` writes source files to CWD and
    /// artifacts to /tmp/x; covered in `engine.rs` integration tests.
    pub async fn run_with_dirs(
        &self,
        name: &str,
        task: String,
        out_dir: Option<PathBuf>,
        code_dir: Option<PathBuf>,
    ) -> Result<WorkflowContext, WorkflowError> {
        self.run_with_perf_and_dirs(name, task, out_dir, code_dir)
            .await
            .map(|(ctx, _perf)| ctx)
    }

    /// Run the workflow and return both the final `WorkflowContext` and the
    /// in-memory `PerfRecord`.
    ///
    /// Why (#151): Callers that need to project the run into a `PmResponse`
    /// JSON envelope need the cost/latency/token totals without re-reading
    /// the flushed perf JSON. Keeping `run` as the narrative API preserves
    /// existing callers and tests.
    /// What: Identical to `run` but surfaces the `PerfRecord` alongside the
    /// context. `run` is now a thin wrapper around this method.
    /// Test: Exercised by `main::run_workflow` end-to-end.
    pub async fn run_with_perf(
        &self,
        name: &str,
        task: String,
        out_dir: Option<PathBuf>,
    ) -> Result<(WorkflowContext, crate::perf::PerfRecord), WorkflowError> {
        // Legacy entry point: code lands alongside artifacts.
        self.run_with_perf_and_dirs(name, task, out_dir, None).await
    }

    /// Same as `run_with_perf` but accepts a separate `code_dir` for generated
    /// source files (#222). When `code_dir` is `None`, falls back to using
    /// `out_dir` for code as well — preserving pre-#222 behavior exactly.
    pub async fn run_with_perf_and_dirs(
        &self,
        name: &str,
        task: String,
        out_dir: Option<PathBuf>,
        code_dir: Option<PathBuf>,
    ) -> Result<(WorkflowContext, crate::perf::PerfRecord), WorkflowError> {
        // #54: Accept either a bare workflow name (joined to `config_dir`) or a
        // literal path (anything ending in `.json` or containing a path
        // separator). Without this, `--workflow config/workflows/foo.json`
        // double-joins to `config/workflows/config/workflows/foo.json`.
        let path = if name.ends_with(".json") || name.contains('/') {
            PathBuf::from(name)
        } else {
            self.config_dir.join(format!("{name}.json"))
        };
        if !path.exists() {
            return Err(WorkflowError::WorkflowNotFound {
                path: path.display().to_string(),
            });
        }
        let def =
            WorkflowDef::load(&path).map_err(|e| WorkflowError::ConfigInvalid(format!("{e:#}")))?;

        if def.phases.is_empty() {
            return Err(WorkflowError::ConfigInvalid(
                "workflow has no phases".to_string(),
            ));
        }

        info!(workflow = %def.name, phases = def.phases.len(), "starting workflow");

        // #84: If a ticket manager is attached, create the tracking issue
        // before any phase runs. Failures are logged and non-fatal — the
        // workflow must not die because GitHub is unreachable.
        let workflow_started = Instant::now();
        let task_preview_full = task.clone();
        if let Some(tm_cell) = &self.ticket_manager {
            let mut tm = tm_cell.lock().await;
            if tm.enabled() {
                if let Err(e) = tm
                    .on_workflow_start(&def.name, self.build, &task_preview_full)
                    .await
                {
                    tracing::warn!(error = %e, "ticket manager: on_workflow_start failed");
                }
                // #84: Best-effort related-issue search using the first line
                // of the task as keywords. Drop silently on failure.
                let keywords = task_preview_full
                    .lines()
                    .next()
                    .unwrap_or(&task_preview_full)
                    .chars()
                    .take(80)
                    .collect::<String>();
                if let Err(e) = tm.auto_relate(&keywords).await {
                    tracing::warn!(error = %e, "ticket manager: auto_relate failed");
                }
            }
        }

        // #47: Start perf collection for the whole run. Each phase's wall
        // clock + aggregated TokenUsage + resolved agent model get pushed
        // into the collector and flushed to disk at the end.
        let mut perf = PerfCollector::new(self.build, &def.name, &task);
        // #84: Accumulate cost + files count so the success hook has a
        // single-line summary without re-reading the flushed perf JSON.
        let mut total_cost_usd: f64 = 0.0;
        let mut files_generated: usize = 0;
        let mut qa_summary: String = "n/a".to_string();
        // #123: Track whether the code phase ran under a `claude-code` runner
        // so we can prepend a path-search hint to the QA agent's prompt.
        // Why: claude-code occasionally writes files to the git repo root
        // instead of `out_dir`. The post-code reconcile step relocates them,
        // but in case any slipped through (e.g. files not listed in
        // assignments.json), QA should know to also search the project root.
        let mut code_phase_used_claude_code: bool = false;

        // #126 bug 3: Pre-create out_dir before any agent runs so subprocess
        // runners can safely set it as CWD without hitting ENOENT on first use.
        //
        // #153: Canonicalize out_dir to an absolute path immediately after
        // creation. Downstream code threads `out_dir` into `RunContext::working_dir`,
        // which `ClaudeCodeAgentRunner` passes to `Command::current_dir`. When
        // the CLI is invoked with a relative `current_dir` (e.g. "out/l5-..."),
        // subtle interactions with the inherited `PWD` env var and the claude
        // CLI's own path resolution can cause file writes to land in the
        // parent process CWD (project root) instead of `out_dir`. Canonicalizing
        // to absolute removes that ambiguity: the subprocess unambiguously uses
        // out_dir regardless of how it resolves relative paths internally.
        // `discover_project_dir` (which scans only immediate children of
        // `out_dir`) then finds the generated project, and QA can locate its
        // files.
        if let Some(dir) = &out_dir {
            tokio::fs::create_dir_all(dir).await.map_err(|e| {
                WorkflowError::ConfigInvalid(format!(
                    "failed to create out_dir {}: {e}",
                    dir.display()
                ))
            })?;
        }
        // Replace `out_dir` with its canonical absolute form. Canonicalize
        // happens *after* the directory exists (std::fs::canonicalize requires
        // the path to exist on disk). If canonicalization fails for any reason,
        // fall back to the original path to avoid breaking the run.
        let out_dir: Option<PathBuf> = out_dir.map(|dir| {
            std::fs::canonicalize(&dir)
                .inspect_err(|e| {
                    tracing::warn!(
                        out_dir = %dir.display(),
                        error = %e,
                        "failed to canonicalize out_dir; using original path"
                    );
                })
                .unwrap_or(dir)
        });

        // #222: Resolve `code_dir` — the destination for generated source files.
        // When unset, falls back to `out_dir` so legacy callers see no behavior
        // change. When set, we pre-create + canonicalize it the same way as
        // out_dir so downstream code (subprocess working_dir, wave-loop joins,
        // file-presence checks) gets an absolute, real path.
        let code_dir: Option<PathBuf> = match code_dir {
            Some(dir) => {
                tokio::fs::create_dir_all(&dir).await.map_err(|e| {
                    WorkflowError::ConfigInvalid(format!(
                        "failed to create code_dir {}: {e}",
                        dir.display()
                    ))
                })?;
                Some(
                    std::fs::canonicalize(&dir)
                        .inspect_err(|e| {
                            tracing::warn!(
                                code_dir = %dir.display(),
                                error = %e,
                                "failed to canonicalize code_dir; using original path"
                            );
                        })
                        .unwrap_or(dir),
                )
            }
            None => out_dir.clone(),
        };
        if let (Some(o), Some(c)) = (out_dir.as_ref(), code_dir.as_ref())
            && o != c
        {
            info!(
                out_dir = %o.display(),
                code_dir = %c.display(),
                "#222: separated artifacts dir (out_dir) from code dir (code_dir)"
            );
        }

        // #196: Detect the active persona from the raw task text BEFORE we
        // hand it off to the workflow context. The persona drives two things:
        //   1. Which phases run (e.g. `[hacker]` skips research/qa/docs).
        //   2. The cleaned task body that every phase sees — the explicit
        //      `[persona]` / `persona:foo` marker is a PM-only signal and must
        //      not leak into downstream agent prompts.
        // We thread `cleaned_task` (not the raw `task`) into the context so
        // every `{{task}}` template substitution downstream is tag-free.
        let (persona, matched_kw) = detect_persona_matched(&task);
        let cleaned_task = strip_persona_tag(&task);
        // Best-effort: surface the detected persona on the event bus so the
        // UI can render the active mode. The session id may be empty when run
        // outside the API server harness; emit anyway so in-process subscribers
        // (e.g. local tests, CLI run) still see it.
        {
            let session_id = std::env::var("OPEN_MPM_RUN_ID").unwrap_or_default();
            crate::events::emit(crate::events::Event::PersonaDetected {
                session_id,
                persona: persona.to_string(),
            });
        }
        info!(persona = %persona, matched = %matched_kw, "workflow persona detected");
        // #205: emit a visible warning when a non-engineer persona will skip
        // phases, so operators can see *why* parts of the pipeline are absent.
        {
            let skipped = phases_to_skip(persona);
            if !skipped.is_empty() {
                let phases_list = skipped.join(", ");
                tracing::warn!(
                    persona = %persona,
                    matched = %matched_kw,
                    skipped = %phases_list,
                    "[open-mpm] persona detected: {persona} (matched: \"{matched_kw}\") — skipping phases: {phases_list}"
                );
                eprintln!(
                    "[open-mpm] persona detected: {persona} (matched: \"{matched_kw}\") — skipping phases: {phases_list}"
                );
            }
        }

        let mut ctx = WorkflowContext::builder(cleaned_task)
            .with_out_dir(out_dir.clone())
            .build();

        // #56: Track the first failing phase name and the error so we can
        // flush a partial perf record before propagating. Previously an early
        // `?` return dropped `perf` without flushing, losing all cost/latency
        // data for failed runs.
        let mut failed_phase: Option<String> = None;
        let mut first_error: Option<WorkflowError> = None;

        // Fix 2 (claude-mpm parity): QA gating state.
        //
        // Why: When the QA agent emits a structured JSON envelope with
        // `status: "fail"` we want to (a) mark the run as having failed at QA
        // for perf bookkeeping, and (b) inject the failure details into the
        // engineer agent on a single retry pass — without restructuring the
        // phase loop or risking an infinite retry. We cap retries at exactly
        // one and only feed back into the next `code` phase that runs after a
        // failed QA.
        // What: `qa_failure_feedback` carries the `[QA FEEDBACK] …` block to
        // prepend to the next code-phase template render. `qa_retry_count`
        // bounds retries (we advance with `failed_phase = "qa"` after the
        // second consecutive failure). Backward-compatible: free-text QA
        // output never trips these.
        let mut qa_failure_feedback: Option<String> = None;
        let mut qa_retry_count: u32 = 0;

        // #173: Run pre-plan skill discovery once for the whole workflow so
        // every skill the engine considered is recorded in the perf record
        // (`skills_considered`) regardless of which phase eventually consumed
        // them. The discovered list is only injected into the plan-agent's
        // prompt below; other phases continue to use the legacy
        // `skills_loader` / `skill_registry` paths unchanged.
        let discovered_skills: Vec<DiscoveredSkill> = self.discover_skills_for_task(&ctx.task, 8);
        for skill in &discovered_skills {
            perf.record_skill_considered(&skill.name);
        }

        // #347 follow-up: Pre-index existing source under `code_dir` before any
        // AST-native phase runs.
        //
        // Why: `--workflow prescriptive` against an existing project starts the
        // AST-native tool surface (`get_symbol`, etc.) with an empty registry.
        // Research/plan agents have no structural view on entry; every lookup
        // pays a per-file disk parse. Walking the tree once up front populates
        // the process-global pre-indexed registry so subsequent tool calls hit
        // a warm cache.
        // What: Detects whether any phase opts into AST-native (or whether the
        // global `--ast-native` override is on). When yes, calls
        // `pre_index_directory` against `code_dir` (the path that holds the
        // user's existing project) and installs the resulting registry via
        // `set_pre_indexed_registry`. Failures are logged and non-fatal — the
        // run continues with an empty registry.
        // Test: Indirectly via `pre_indexed_registry_round_trip` in `src/ast/mod.rs`
        // and the workflow integration suite.
        let any_ast_phase = def.phases.iter().any(|p| p.ast_native == Some(true))
            || crate::ast::is_ast_native_overridden();
        if any_ast_phase && let Some(dir) = code_dir.as_ref().filter(|d| d.exists()) {
            let started = std::time::Instant::now();
            match crate::ast::pre_index_directory(dir, dir) {
                Ok(registry) => {
                    let symbol_count = registry.len();
                    crate::ast::set_pre_indexed_registry(registry);
                    info!(
                        duration_ms = started.elapsed().as_millis() as u64,
                        symbols = symbol_count,
                        path = %dir.display(),
                        "AST pre-index complete"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        path = %dir.display(),
                        "AST pre-index failed, continuing with empty registry"
                    );
                }
            }
        }

        'phase_loop: for phase in &def.phases {
            // #82: Phases with `skip: true` are opt-out (the `docs` phase
            // ships disabled by default). No agent runs, no output is
            // recorded, no file extraction happens — just log and continue.
            if phase.skip.unwrap_or(false) {
                info!(phase = %phase.name, agent = %phase.agent, "skipping phase (skip=true)");
                // #209: Write a sentinel string so downstream templates that
                // reference {{phase_name}} render a clear "skipped" message
                // instead of `(missing: phase_name)`. This lets the observe
                // agent distinguish "phase was skipped" from "phase failed"
                // or "phase was never configured".
                ctx.record_phase(
                    &phase.name,
                    "(skipped: disabled in workflow config)".to_string(),
                    None,
                );
                continue;
            }
            // #196: Persona-driven phase skipping. `phases_to_skip(persona)`
            // returns the static list of phase names this persona opts out of.
            // The check happens BEFORE the agent is spawned (no work, no cost,
            // no perf entry — just an info log + a `PhaseSkipped` event for
            // observability). The default `engineer` persona has an empty skip
            // list, so legacy behaviour is preserved exactly.
            if phases_to_skip(persona).contains(&phase.name.as_str()) {
                info!(
                    phase = %phase.name,
                    persona = %persona,
                    "skipping phase (persona opt-out)"
                );
                let session_id = std::env::var("OPEN_MPM_RUN_ID").unwrap_or_default();
                crate::events::emit(crate::events::Event::PhaseSkipped {
                    session_id,
                    phase: phase.name.clone(),
                    persona: persona.to_string(),
                });
                // #209: Write a sentinel string so downstream templates that
                // reference {{phase_name}} render a meaningful message rather
                // than `(missing: phase_name)`.
                ctx.record_phase(
                    &phase.name,
                    format!("(skipped: {} persona does not run this phase)", persona),
                    None,
                );
                continue;
            }
            info!(phase = %phase.name, agent = %phase.agent, "running phase");

            // Per-phase AST-native override (#347/#348 follow-up).
            //
            // Why: Bake-off data shows AST-native is -55-60% cheaper for the
            // research and plan phases but +20% more expensive for code/qa
            // (with fewer tests generated). A hybrid mode — AST-native for
            // research+plan, traditional for code+qa — yields ~14% total
            // cost reduction with no quality loss. The override lives in a
            // process-wide AtomicBool, so we save the prior value, apply the
            // phase's preference, and let `_ast_guard` restore it when the
            // phase completes (Drop runs on every continue / break / error).
            // What: When `phase.ast_native` is `Some(v)`, set the global
            // override to `v` for the duration of this phase. When it's
            // `None`, leave the global state untouched (inherits the
            // `--ast-native` CLI flag).
            // Test: Indirectly via end-to-end bake-off; per-phase parsing
            // verified by `phase_ast_native_parses_from_json`.
            struct AstNativeGuard {
                prev: bool,
                applied: bool,
            }
            impl Drop for AstNativeGuard {
                fn drop(&mut self) {
                    if self.applied {
                        crate::ast::set_ast_native_override(self.prev);
                    }
                }
            }
            let _ast_guard = {
                let prev = crate::ast::is_ast_native_overridden();
                let applied = if let Some(phase_ast) = phase.ast_native {
                    crate::ast::set_ast_native_override(phase_ast);
                    true
                } else {
                    false
                };
                AstNativeGuard { prev, applied }
            };

            // #149: Stream phase-start to stderr so operators see live
            // progress; also emit a machine-readable progress event for the
            // API server to forward to the Tauri UI.
            if let Some(rep) = &self.progress {
                rep.phase_start(&phase.name);
            }
            emit_progress_event(&phase.name, "running", 0.0, 0.0, None);

            // #140: Before rendering the phase template, refresh the
            // discovered project_dir. The code phase may have just written
            // files into a subdirectory of out_dir (e.g. out/task_board/
            // containing pyproject.toml). QA and docs templates that use
            // {{project_dir}} need to see that subdirectory, not out_dir,
            // so pytest runs where tests/ actually lives.
            // #222: Discover the project root inside `code_dir` (where source
            // files actually land) rather than `out_dir` (artifacts). When the
            // two are the same (legacy mode), behavior is unchanged.
            if let Some(dir) = code_dir.as_deref() {
                ctx.project_dir = discover_project_dir(dir);
                if let Some(p) = &ctx.project_dir
                    && p.as_path() != dir
                {
                    info!(
                        project_dir = %p.display(),
                        code_dir = %dir.display(),
                        "discovered project subdirectory for {{{{project_dir}}}}"
                    );
                }
            }

            let mut rendered = ctx.render_template(&phase.context_template);

            // Fix 2 (claude-mpm parity): If the previous QA phase emitted a
            // structured `status: "fail"` envelope and we have not yet
            // exhausted the retry cap, prepend the QA feedback block to the
            // next code-phase render so the engineer agent can address the
            // failure. Consume (take) the feedback so it only fires once per
            // failure.
            if phase.name == "code"
                && let Some(feedback) = qa_failure_feedback.take()
            {
                tracing::info!(
                    "code phase: prepending QA feedback to engineer prompt (one-shot retry)"
                );
                rendered = format!("{feedback}{rendered}");
            }

            // #123: When the code phase ran under a `claude-code` runner,
            // prepend a path-search hint to the QA prompt so QA knows to
            // also look at the git repository root if files are missing
            // from `out_dir` / `project_dir`. The post-code reconcile step
            // already moved files listed in `assignments.json`, but this
            // catches anything not declared (e.g. ad-hoc test fixtures).
            if phase.name == "qa" && code_phase_used_claude_code {
                let project_root = std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                let hint = format!(
                    "## Note: claude-code runner was used for the code phase\n\n\
                     If the project files are not visible at the expected location, \
                     also check the project root: `{project_root}`. The claude CLI \
                     occasionally writes relative paths against the git repository \
                     root instead of the workflow `out_dir`. Files matching the \
                     assignments plan have already been relocated, but ad-hoc \
                     fixtures may still live at `{project_root}`.\n\n---\n\n"
                );
                rendered = format!("{hint}{rendered}");
            }

            // #108/#109: Prepend the project self-init prefix (project index
            // summary) to every phase's task. Empty when init produced no
            // content, so this is a no-op otherwise.
            //
            // Relevance-first context injection: filter the project_summary
            // bullet list by keyword overlap with the user task before
            // injection so we don't burn tokens on irrelevant index entries
            // for narrowly-scoped tasks. Falls back to first-N entries when
            // no keywords match anything (preserves prior wholesale-injection
            // semantics as a safety net).
            if let Some(init) = &self.init_context {
                let mut filtered_init = init.clone();
                filtered_init.project_summary = crate::agents::context_filter::filter_index_entries(
                    &init.project_summary,
                    &ctx.task,
                    15,
                );
                let prefix = filtered_init.to_prompt_prefix();
                if !prefix.is_empty() {
                    rendered = format!("{prefix}{rendered}");
                }
            }

            // #173: For the `plan` phase, prepend a `## Available Skills`
            // summary block listing every skill the engine matched against
            // the task signals. Replaces the manual `list_skills` tool call
            // the plan-agent used to make — the planner now sees every
            // candidate skill (with a one-line summary + tags) up front so
            // it can reason about ecosystem compatibility before writing
            // `assignments.json`. No-op when discovery returned nothing or
            // the phase isn't `plan`.
            if phase.name == "plan" && !discovered_skills.is_empty() {
                let mut block =
                    String::from("## Available Skills (auto-matched for this task)\n\n");
                for skill in &discovered_skills {
                    let tag_csv = skill.tags.join(", ");
                    block.push_str(&format!(
                        "- **{}** [tags: {}] — {}\n",
                        skill.name, tag_csv, skill.summary
                    ));
                }
                block.push_str(
                    "\nSkills have been pre-loaded above. Use them. \
                     Call `load_skill(name=\"<name>\")` only if you need the full body.\n",
                );
                tracing::debug!(
                    phase = %phase.name,
                    count = discovered_skills.len(),
                    "pre-plan skill discovery injected into plan-agent prompt"
                );
                rendered = format!("{block}\n---\n\n{rendered}");
            }

            // Skills-first: If a SkillsLoader is attached, use it to detect and inject
            // language/framework skills based on the project directory and task text.
            // This supersedes the legacy skill_registry auto_inject path when both are present.
            if let Some(loader) = self.skills_loader.as_ref() {
                // #233: Scope language/framework skill detection to the task's
                // own output directory (where the agent's source files land),
                // not the harness's CWD. Using `current_dir()` resolved to the
                // open-mpm repo root, which contains `Cargo.toml`, causing the
                // "rust" skill to be injected into every task regardless of
                // the task's actual language. Prefer `code_dir`, fall back to
                // `out_dir`, and only use CWD if neither is configured (the
                // legacy in-process test path).
                let project_dir =
                    code_dir
                        .clone()
                        .or_else(|| out_dir.clone())
                        .unwrap_or_else(|| {
                            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
                        });
                let explicit_skills: Vec<String> = phase.skills.as_deref().unwrap_or(&[]).to_vec();
                // #171: Use the tracked variant so the perf collector can
                // record exactly which skills were injected for post-run
                // effectiveness/usage updates.
                let (skills_prefix, used_names) = loader
                    .build_skills_prefix_tracked(&explicit_skills, &project_dir, &rendered)
                    .await;
                if !skills_prefix.is_empty() {
                    tracing::debug!(
                        phase = %phase.name,
                        skills_chars = skills_prefix.len(),
                        skills = ?used_names,
                        "skills-loader injected skills into phase task"
                    );
                    for name in &used_names {
                        perf.record_skill_used(name);
                    }
                    rendered = format!("{skills_prefix}\n\n---\n\n{rendered}");
                }
            }

            // #81: Auto-inject relevant skill bodies as a prompt prefix so the
            // agent has domain knowledge without having to discover it via a
            // tool call. Gracefully empty when the registry has no matches.
            if let Some(reg) = self.skill_registry.as_ref()
                && !reg.is_empty()
            {
                let skills_prefix = reg.auto_inject(&rendered, self.max_auto_skills).await;
                if !skills_prefix.is_empty() {
                    tracing::debug!(
                        phase = %phase.name,
                        skills_chars = skills_prefix.len(),
                        "auto-injected skills into phase task"
                    );
                    rendered = format!("{skills_prefix}\n\n---\n\n{rendered}");
                }
            }

            // #68: If we have a parsed goal block (emitted by the planner),
            // prepend its header to the task text for every downstream phase.
            // This keeps the goals anchored at the top of each agent's input
            // without requiring IPC protocol changes.
            if let Some(goal) = &ctx.goal_block
                && !goal.is_empty()
            {
                rendered = format!("{}\n\n---\n\n{}", goal.to_prompt_header(), rendered);
            }

            // #118: Append user-scoped memory at the lowest priority (after
            // project context, skills, and goal block). This ensures
            // cross-project learnings from ~/.open-mpm/memory/ enrich the
            // prompt without displacing higher-priority project signals.
            if let Some(suffix) = &self.user_memory_suffix
                && !suffix.is_empty()
            {
                rendered.push_str(suffix);
            }

            let phase_started = Instant::now();

            // #107: `phase.model` is threaded through `RunContext`
            // below so claude-code and subprocess runners both see the
            // per-phase model. Previously this was only logged ("advisory")
            // and the claude-code runner always used the agent TOML's model.
            if let Some(m) = &phase.model {
                tracing::debug!(phase = %phase.name, model = %m, "phase model");
            }

            // #51: Check the agent's TOML for `persistent_session`. If it is
            // enabled, fetch any prior session history in wire format, pass
            // it through the runner, and record the new exchange on success.
            // Failing to load the config is not fatal — we fall back to
            // stateless behavior (matches pre-#51 semantics) so a missing or
            // malformed TOML cannot brick a workflow that doesn't depend on
            // persistent sessions.
            let persistent = AgentConfig::by_name(&phase.agent)
                .map(|c| c.agent.persistent_session)
                .unwrap_or(false);

            // #88: Wave-loop execution path. When this is the "code" phase AND
            // the plan-agent has written `assignments.json` into `out_dir`,
            // run one code-agent per file in topological wave order instead of
            // a single monolithic invocation. Falls through to the normal
            // single-agent path when assignments.json is absent, preserving
            // backward compatibility with workflows that never opt in.
            //
            // #88 follow-up: Explicitly log the decision at the code phase so
            // live runs show which path was taken. Previously the only signal
            // was "1 vs N code-agent spawns", which made diagnosing a silent
            // parse failure painful. `Assignments::load` itself logs the
            // specific reason (absent / read-fail / parse-fail / ok); this log
            // summarizes the branch at the engine level.
            let wave_assignments = if phase.name == "code" {
                match out_dir.as_deref() {
                    Some(dir) => {
                        // CRIT-1 / MAJ-1 (#90, #93): The plan-agent's
                        // subprocess is now spawned with `out_dir` as its CWD
                        // (via `RunContext.working_dir`), so its Write-tool
                        // output lands directly in `out_dir`. No cwd-fallback
                        // copy needed; the primary check is authoritative.
                        let loaded = Assignments::load(dir);
                        if loaded.is_some() {
                            tracing::info!(
                                phase = %phase.name,
                                out_dir = %dir.display(),
                                "code phase: taking WAVE LOOP path (assignments.json present)"
                            );
                        } else {
                            tracing::info!(
                                phase = %phase.name,
                                out_dir = %dir.display(),
                                "code phase: taking LEGACY monolithic path (assignments.json absent or unparseable)"
                            );
                        }
                        loaded
                    }
                    None => {
                        tracing::debug!(
                            phase = %phase.name,
                            "code phase: no out_dir configured; wave loop cannot run"
                        );
                        None
                    }
                }
            } else {
                None
            };

            // Per-phase AST-native override (#349 follow-up to #348).
            //
            // Why: Bake-off data across L1+L2 shows AST-native is -55-60%
            // cheaper in research/plan phases with no quality loss but +20%
            // more expensive in code/qa phases with fewer tests generated.
            // A hybrid mode (AST-native for research+plan, traditional for
            // code+qa) yields ~14% total cost reduction. Per-phase override
            // lets workflow JSON encode this hybrid declaratively without
            // requiring CLI flag gymnastics.
            // What: Save the current global override, apply the phase's
            // override (if any) for the duration of the dispatch, then
            // restore the global value in BOTH the Ok and Err paths so
            // subsequent phases see the correct inherited value.
            let prior_ast_native = crate::ast::is_ast_native_overridden();
            if let Some(phase_ast) = phase.ast_native {
                crate::ast::set_ast_native_override(phase_ast);
            }

            let output_result: Result<AgentOutput, WorkflowError> = if let Some(asg) =
                wave_assignments
            {
                let artifacts_dir = out_dir
                    .as_deref()
                    .expect("assignments.json only loaded when out_dir is Some");
                // #222: Wave loop writes code to `code_dir` (which == out_dir
                // in legacy mode). It also reads stubs from `out_dir/stubs/`.
                let code_target = code_dir.as_deref().unwrap_or(artifacts_dir);
                run_wave_loop(
                    asg,
                    phase,
                    self.agent_runner.clone(),
                    code_target,
                    artifacts_dir,
                    self.progress.clone(),
                )
                .await
                .map_err(|e| WorkflowError::PhaseFailed {
                    phase: phase.name.clone(),
                    source: e,
                })
            } else if let Some(subs) = phase.parallel_subtasks.as_ref().filter(|s| !s.is_empty()) {
                let use_worktrees = phase.worktree_protection.unwrap_or(false);
                // Parallel sub-agents write into per-label subdirs under the
                // out_dir (or a temp dir if no out_dir is set).
                let phase_out_dir = out_dir
                    .clone()
                    .unwrap_or_else(|| std::env::temp_dir().join("open-mpm-parallel"));
                match run_parallel_phase(
                    &rendered,
                    subs,
                    &phase.agent,
                    self.agent_runner.clone(),
                    &phase_out_dir,
                    use_worktrees,
                )
                .await
                {
                    Ok(results) if !results.is_empty() => {
                        // Merge file trees into the phase_out_dir via ConflictResolver.
                        let api_key = std::env::var("OPENROUTER_API_KEY").unwrap_or_default();
                        let resolver = ConflictResolver::new(api_key);
                        let merge_report = resolver
                            .merge(&results, &phase_out_dir)
                            .await
                            .unwrap_or_else(|e| format!("merge failed: {e:#}"));

                        // Aggregate combined content + usage across sub-agents.
                        let mut combined = String::new();
                        let mut agg_usage = crate::perf::TokenUsage::default();
                        for r in &results {
                            combined.push_str(&format!("## Sub-agent [{}]\n\n", r.label));
                            combined.push_str(&r.output.content);
                            combined.push_str("\n\n");
                            agg_usage.add(&r.output.usage);
                        }
                        combined.push_str("\n---\n\n");
                        combined.push_str(&merge_report);

                        Ok(AgentOutput {
                            content: combined,
                            summary: None,
                            usage: agg_usage,
                        })
                    }
                    Ok(_) => Err(WorkflowError::PhaseFailed {
                        phase: phase.name.clone(),
                        source: anyhow::anyhow!(
                            "parallel phase '{}' produced zero successful sub-agent results",
                            phase.name
                        ),
                    }),
                    Err(e) => Err(WorkflowError::PhaseFailed {
                        phase: phase.name.clone(),
                        source: e,
                    }),
                }
            } else if persistent {
                // Bug #122: build ctx before the persistent branch so
                // working_dir and model reach run_with_history too.
                // #410: All phases run with `working_dir = code_dir` (the
                // user's project source tree). Previously non-code phases
                // used `out_dir` (artifacts), which meant agents like QA
                // couldn't read project source files. Artifacts written by
                // the harness (assignments.json, workflow-report.md, perf
                // records, stubs/) are absolute paths under `out_dir` and
                // are unaffected by the agent CWD. When `code_dir` is
                // unset (no `--project-dir` and no auto-default), fall
                // back to `out_dir` to preserve legacy single-dir mode.
                let working_dir = code_dir.clone().or_else(|| out_dir.clone());
                let ctx = RunContext {
                    working_dir,
                    model: phase.model.clone(),
                    ..RunContext::default()
                };
                let history = self.sessions.get_history_wire(&phase.agent).await;
                match self
                    .agent_runner
                    .run_with_history(&phase.agent, &rendered, &history, &ctx)
                    .await
                {
                    Ok(out) => {
                        // Record exchange so the next call to this agent sees it.
                        if let Err(e) = self
                            .sessions
                            .extend_history(&phase.agent, &rendered, &out.content)
                            .await
                        {
                            tracing::warn!(agent = %phase.agent, error = %e, "failed to extend session history; continuing");
                        }
                        Ok(out)
                    }
                    Err(e) => Err(WorkflowError::PhaseFailed {
                        phase: phase.name.clone(),
                        source: e,
                    }),
                }
            } else {
                // MAJ-1 (#93): Route non-wave calls through `run_with_context`
                // with an explicit `working_dir` so subprocess-driven runners
                // (especially `ClaudeCodeAgentRunner`) write their output
                // somewhere predictable rather than the parent's cwd.
                // #410: All phases run with `working_dir = code_dir` (the
                // user's project source tree). Previously non-code phases
                // used `out_dir` (artifacts), which meant agents like QA
                // couldn't read project source files. Artifacts the harness
                // writes (assignments.json, workflow-report.md, perf records,
                // stubs/) use absolute `out_dir` paths and are unaffected by
                // the agent CWD. When `code_dir` is unset, we fall back to
                // `out_dir` to preserve legacy single-dir mode.
                let working_dir = code_dir.clone().or_else(|| out_dir.clone());
                let ctx = RunContext {
                    working_dir,
                    model: phase.model.clone(),
                    ..RunContext::default()
                };
                self.agent_runner
                    .run_with_context(&phase.agent, &rendered, &ctx)
                    .await
                    .map_err(|e| WorkflowError::PhaseFailed {
                        phase: phase.name.clone(),
                        source: e,
                    })
            };

            // Restore the global AST-native override now that this phase's
            // dispatch is complete (regardless of Ok/Err). This runs before
            // the error handling below so even early-returning phases observe
            // the correct global state on the next iteration.
            crate::ast::set_ast_native_override(prior_ast_native);

            let output = match output_result {
                Ok(o) => o,
                Err(e) => {
                    // #56: Record the duration of the failed phase and capture
                    // the error so we can flush perf before propagating.
                    let duration_ms = phase_started.elapsed().as_millis() as u64;
                    let phase_model = phase
                        .model
                        .clone()
                        .or_else(|| {
                            AgentConfig::by_name(&phase.agent)
                                .ok()
                                .map(|c| c.agent.model)
                        })
                        .unwrap_or_else(|| "unknown".to_string());
                    perf.record_phase(
                        &phase.name,
                        duration_ms,
                        &phase_model,
                        &crate::perf::TokenUsage::default(),
                    );

                    // #84: Post the failed-phase comment too (marked with ❌).
                    if let Some(tm_cell) = &self.ticket_manager {
                        let tm = tm_cell.lock().await;
                        if let Err(err) = tm
                            .on_phase_complete(
                                &phase.name,
                                &phase_model,
                                duration_ms,
                                0,
                                0,
                                0.0,
                                "❌ failed",
                            )
                            .await
                        {
                            tracing::warn!(error = %err, phase = %phase.name,
                                "ticket manager: on_phase_complete (failure) failed");
                        }
                    }

                    // #149: Stream the failure to stderr + machine progress event.
                    let elapsed = std::time::Duration::from_millis(duration_ms);
                    let err_msg = format!("{e}");
                    if let Some(rep) = &self.progress {
                        rep.phase_failed(&phase.name, elapsed, &err_msg);
                    }
                    emit_progress_event(
                        &phase.name,
                        "failed",
                        elapsed.as_secs_f32(),
                        0.0,
                        Some(&err_msg),
                    );

                    failed_phase = Some(phase.name.clone());
                    first_error = Some(e);
                    break 'phase_loop;
                }
            };

            // #27: Prefer the agent-supplied summary (from the IPC Result
            // variant). If absent, extract one ourselves from the content so
            // downstream phase templates still get a bounded digest.
            let summary = output
                .summary
                .clone()
                .or_else(|| Some(extract_summary(&output.content)));
            let content_len = output.content.len();
            let summary_len = summary.as_ref().map(|s| s.len()).unwrap_or(0);

            // #47: Record perf BEFORE we move `output.content` into the ctx
            // so we still have access to the model + usage.
            let duration_ms = phase_started.elapsed().as_millis() as u64;
            // Resolve the model: prefer phase override, fall back to the
            // agent's config, fall back to "unknown" (pricing defaults to
            // Sonnet when model name doesn't match a known prefix).
            let phase_model = phase
                .model
                .clone()
                .or_else(|| {
                    AgentConfig::by_name(&phase.agent)
                        .ok()
                        .map(|c| c.agent.model)
                })
                .unwrap_or_else(|| "unknown".to_string());
            perf.record_phase(&phase.name, duration_ms, &phase_model, &output.usage);

            // #84: Compute this phase's cost for the ticket comment and
            // accumulate into the workflow total. Mirrors the logic the
            // perf collector uses internally.
            let phase_cost = crate::perf::cost_usd(
                &phase_model,
                output.usage.prompt_tokens,
                output.usage.completion_tokens,
                output.usage.cache_read_tokens,
                output.usage.cache_creation_tokens,
            );
            total_cost_usd += phase_cost;

            // #149: Stream phase-done to stderr + machine progress event.
            // For QA, surface the first content line as a note (e.g.
            // "35/35 passed").
            let phase_note: Option<String> = if phase.name == "qa" {
                output
                    .content
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .map(|l| l.chars().take(60).collect::<String>())
            } else {
                None
            };
            let elapsed = std::time::Duration::from_millis(duration_ms);
            if let Some(rep) = &self.progress {
                rep.phase_done(
                    &phase.name,
                    elapsed,
                    phase_cost as f32,
                    phase_note.as_deref(),
                );
            }
            emit_progress_event(
                &phase.name,
                "done",
                elapsed.as_secs_f32(),
                phase_cost as f32,
                phase_note.as_deref(),
            );

            // #84: Post per-phase ticket comment. Non-fatal on failure.
            if let Some(tm_cell) = &self.ticket_manager {
                let tm = tm_cell.lock().await;
                if let Err(e) = tm
                    .on_phase_complete(
                        &phase.name,
                        &phase_model,
                        duration_ms,
                        output.usage.prompt_tokens,
                        output.usage.completion_tokens,
                        phase_cost,
                        "✅ success",
                    )
                    .await
                {
                    tracing::warn!(error = %e, phase = %phase.name,
                        "ticket manager: on_phase_complete failed");
                }
            }

            // #84: If this is the QA phase, grab a short summary for the
            // success hook. The content is the agent's raw output; pulling
            // the first non-empty line is a reasonable default.
            //
            // Fix 2 (claude-mpm parity): If the QA agent emits a structured
            // JSON envelope `{"status":"pass|fail","passed":N,"failed":N,
            // "errors":[…],"details":"…"}` we additionally:
            //   • record exact pass/fail counts on the perf record,
            //   • on `fail`, mark `failed_phase = "qa"` for the run record,
            //   • on `fail` (first occurrence), stash a `[QA FEEDBACK] …`
            //     block to prepend to the next code-phase render so the
            //     engineer can fix the failure on retry,
            //   • cap retries at exactly one — a second consecutive failure
            //     advances the workflow with `failed_phase = "qa"` recorded.
            // Free-text QA output continues to flow through the legacy
            // first-line-as-summary path with no behavior change.
            if phase.name == "qa" {
                qa_summary = output
                    .content
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .map(|l| l.chars().take(80).collect::<String>())
                    .unwrap_or_else(|| "completed".to_string());

                if let Some(env) = parse_qa_envelope(&output.content) {
                    if let (Some(p), Some(f)) = (env.passed, env.failed) {
                        perf.set_test_counts(p, f);
                    } else if let Some(p) = env.passed {
                        perf.set_test_counts(p, 0);
                    } else if let Some(f) = env.failed {
                        perf.set_test_counts(0, f);
                    }

                    match env.status {
                        QaStatus::Pass => {
                            // Clear any stale feedback from a prior failure
                            // that the engineer subsequently fixed.
                            qa_failure_feedback = None;
                        }
                        QaStatus::Fail => {
                            // Mark the run as having failed at QA, but do
                            // not break the loop — observe / docs may still
                            // need to run, and we do not want to mask the
                            // perf record.
                            if failed_phase.is_none() {
                                failed_phase = Some("qa".to_string());
                            }
                            if qa_retry_count < 1 {
                                qa_retry_count += 1;
                                let detail = env.details.clone().unwrap_or_else(|| {
                                    format!(
                                        "QA reported {} failed test(s).",
                                        env.failed.unwrap_or(0)
                                    )
                                });
                                qa_failure_feedback = Some(format!(
                                    "[QA FEEDBACK] Tests failed in previous run. Details:\n\
                                     {detail}\n\
                                     Please fix these failures before proceeding.\n\n"
                                ));
                                tracing::warn!(
                                    retry = qa_retry_count,
                                    "QA reported status=fail; queued feedback for next code phase"
                                );
                            } else {
                                tracing::warn!(
                                    "QA reported status=fail again after retry cap; advancing"
                                );
                                qa_failure_feedback = None;
                            }
                        }
                    }
                }
            }

            // #70: Fire-and-forget record this turn for background indexing.
            // Runs before we move `output.content` into the context.
            if let Some(idx) = &self.indexer {
                let run_id =
                    std::env::var("OPEN_MPM_RUN_ID").unwrap_or_else(|_| "unknown".to_string());
                idx.record(TurnRecord {
                    session_id: run_id,
                    agent: phase.agent.clone(),
                    turn_number: ctx.phase_outputs.len() as u32,
                    timestamp: chrono::Utc::now(),
                    prompt_text: rendered.clone(),
                    response_text: output.content.clone(),
                    prompt_tokens: output.usage.prompt_tokens,
                    completion_tokens: output.usage.completion_tokens,
                });
            }

            ctx.record_phase(&phase.name, output.content, summary);

            // #68: After the plan phase, try to extract the goal block so
            // downstream phases can inject it into their prompts. We look for
            // a ```goal ... ``` fence in the phase output. Missing is fine —
            // workflows without a planner keep working unchanged.
            if ctx.goal_block.is_none()
                && let Some(content) = ctx.phase_outputs.get(&phase.name)
                && let Some(g) = crate::context::goals::parse_goal_block_from_text(content)
            {
                tracing::info!(
                    phase = %phase.name,
                    primary = %g.primary,
                    secondary_count = g.secondary.len(),
                    "parsed goal block from phase output"
                );
                ctx.goal_block = Some(g);
            }
            info!(
                phase = %phase.name,
                content_chars = content_len,
                summary_chars = summary_len,
                duration_ms,
                prompt_tokens = output.usage.prompt_tokens,
                completion_tokens = output.usage.completion_tokens,
                cache_read = output.usage.cache_read_tokens,
                cache_creation = output.usage.cache_creation_tokens,
                "phase complete"
            );

            // #160: After the plan phase, check for a misrouted
            // assignments.json at the project root. The plan-agent's claude
            // CLI anchors relative Write-tool paths to the git repository
            // root (its inherited CWD), not to `RunContext::working_dir`. If
            // it wrote there instead of out_dir, relocate the file so the
            // code phase's wave-loop check finds it. Gated on recency
            // (10-minute mtime window) to avoid picking up stale leftovers
            // from prior runs that were never cleaned up.
            if phase.name == "plan"
                && let Some(dir) = out_dir.as_deref()
                && let Err(e) = relocate_plan_outputs_from_project_root(dir).await
            {
                tracing::warn!(
                    error = %e,
                    "post-plan relocation check failed; continuing"
                );
            }

            // #123: After the code phase for a claude-code runner, reconcile
            // file locations. The claude CLI sometimes writes relative paths
            // against the git repository root (its inherited CWD) instead of
            // `RunContext::working_dir`, so files listed in `assignments.json`
            // can land at `project_root/<file>` rather than
            // `out_dir/<file>`. QA then runs against `out_dir` (or the
            // discovered `project_dir` inside it) and reports false failures
            // because no files are present. This step walks the assignments
            // and, for any file missing in `out_dir` but present at the
            // project root with a recent mtime, moves it into place.
            if phase.name == "code" && agent_uses_claude_code(&phase.agent) {
                code_phase_used_claude_code = true;
                // #222: Reconcile against `code_dir` (where source files
                // belong) using `out_dir` as the source of truth for the
                // assignments manifest. When the two paths are identical
                // (legacy mode), behavior is unchanged.
                let target = code_dir.as_deref().or(out_dir.as_deref());
                let assignments_dir = out_dir.as_deref().or(target);
                if let (Some(target), Some(asg_dir)) = (target, assignments_dir)
                    && let Err(e) = reconcile_code_outputs_against(asg_dir, target).await
                {
                    tracing::warn!(
                        error = %e,
                        "post-code reconciliation check failed; continuing"
                    );
                }
            }

            // #64: Extract `## File: <path>` sections from this phase's output
            // to `out_dir` BEFORE the next phase runs. Critical for workflows
            // where the QA phase invokes pytest against files written by the
            // code phase — previously extraction only happened AFTER the whole
            // workflow finished, so QA always ran against an empty directory.
            // #222: For the `code` phase, extracted files go to `code_dir`
            // (the user's project tree). For other produces_files phases
            // (rare), they remain artifacts and go to `out_dir`.
            let extract_target = if phase.name == "code" {
                code_dir.as_deref().or(out_dir.as_deref())
            } else {
                out_dir.as_deref()
            };
            if phase.produces_files.unwrap_or(false)
                && let Some(dir) = extract_target
                && let Some(phase_content) = ctx.phase_outputs.get(&phase.name)
            {
                let files = extract_files_from_content(phase_content);
                let file_count = files.len();
                files_generated += file_count;
                for (rel, body) in &files {
                    let dest = dir.join(rel);
                    if let Some(parent) = dest.parent() {
                        tokio::fs::create_dir_all(parent)
                            .await
                            .map_err(WorkflowError::Io)?;
                    }
                    tokio::fs::write(&dest, body.as_bytes())
                        .await
                        .map_err(WorkflowError::Io)?;
                    info!(
                        file = %dest.display(),
                        phase = %phase.name,
                        "extracted file from phase output"
                    );
                }
                info!(
                    count = file_count,
                    phase = %phase.name,
                    out_dir = %dir.display(),
                    "extracted phase files to out_dir before next phase"
                );
            }
        }

        // Only write the workflow report when all phases succeeded — a partial
        // run's `observe` section would reflect an incomplete workflow.
        if first_error.is_none()
            && let Some(dir) = &out_dir
            && let Some(report) = ctx.phase_outputs.get("observe").cloned()
        {
            let target = dir.join("workflow-report.md");
            if let Some(parent) = target.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(WorkflowError::Io)?;
            }
            if let Err(e) = tokio::fs::write(&target, report).await {
                tracing::warn!(path = %target.display(), error = %e, "workflow report write failed (non-fatal)");
            } else {
                info!(path = %target.display(), "wrote workflow report");
            }
        }

        // #56: Record run status before flushing so partial/failed runs land on
        // disk with the correct outcome.
        if first_error.is_some() {
            perf.set_status("partial");
            if let Some(name) = &failed_phase {
                perf.set_failed_phase(name);
            }
        } else {
            perf.set_status("success");
        }

        // #47 + #56: Flush perf record regardless of success/failure so we
        // always capture latency+token usage for every run. Non-fatal: a perf
        // write failure must not override the original phase error.
        if let Some(perf_dir) = &self.perf_dir
            && let Err(e) = perf.flush(perf_dir).await
        {
            tracing::warn!(error = %e, "failed to flush perf record");
        }
        // #151: snapshot the perf record for in-process consumers (the
        // JSON envelope builder). Cheap clone; `build_record` already
        // duplicates the phase vec.
        let perf_record = perf.build_record();

        // #76: Auto-push on successful workflow completion. Gated on
        // `auto_push.enabled` in the workflow config; a missing config is
        // equivalent to disabled. Failures are non-fatal — the workflow itself
        // already succeeded; we just log.
        if first_error.is_none()
            && let Some(auto_push_cfg) = &def.auto_push
            && auto_push_cfg.enabled
        {
            let push_dir = out_dir
                .clone()
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
            let task_preview = ctx.task.lines().next().unwrap_or(&ctx.task).to_string();
            if let Err(e) = autopush::run_auto_push(
                auto_push_cfg,
                &push_dir,
                &def.name,
                self.build,
                &task_preview,
            )
            .await
            {
                tracing::warn!(error = %e, "auto-push failed (non-fatal)");
            }
        }

        // #84: Fire the final ticket hook — success closes the issue, failure
        // leaves it open with a comment. Both paths log and continue on error.
        if let Some(tm_cell) = &self.ticket_manager {
            let tm = tm_cell.lock().await;
            let total_duration_ms = workflow_started.elapsed().as_millis() as u64;
            if let Some(err) = &first_error {
                let failed = failed_phase.clone().unwrap_or_else(|| "unknown".into());
                if let Err(e) = tm
                    .on_workflow_failure(&failed, &format!("{err}"), self.perf_dir.as_deref())
                    .await
                {
                    tracing::warn!(error = %e, "ticket manager: on_workflow_failure failed");
                }
            } else if let Err(e) = tm
                .on_workflow_success(
                    total_cost_usd,
                    total_duration_ms,
                    &qa_summary,
                    files_generated,
                )
                .await
            {
                tracing::warn!(error = %e, "ticket manager: on_workflow_success failed");
            }
        }

        // #149: Final workflow-complete summary line (only on full success).
        if first_error.is_none()
            && let Some(rep) = &self.progress
        {
            let total_elapsed = workflow_started.elapsed();
            rep.workflow_done(total_elapsed, total_cost_usd as f32);
        }

        if let Some(err) = first_error {
            return Err(err);
        }

        Ok((ctx, perf_record))
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
