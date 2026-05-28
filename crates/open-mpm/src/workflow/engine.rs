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
use crate::workflow::config::{Assignments, PhaseDef, WorkflowDef, safe_join};
use crate::workflow::context::WorkflowContext;
use crate::workflow::error::WorkflowError;
use crate::workflow::parallel::run_parallel_phase;
use crate::workflow::resolver::ConflictResolver;
use crate::workflow::tickets::TicketManager;

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

/// Emit a machine-readable progress event to stderr (#149).
///
/// Why: The HTTP API server (`src/api/server.rs`) spawns `open-mpm` as a
/// subprocess and only sees stdout (the final JSON envelope) and stderr
/// (logging). To surface live phase progress to the Tauri UI poller, we emit
/// a single-line, prefix-tagged JSON record on stderr per phase event so the
/// server can parse those lines out of the stream and update the stored
/// `PmResponse.phases_completed` in real time. Other stderr lines pass
/// through unchanged.
/// What: Writes `__OMPM_PROGRESS__ {<json>}\n` to stderr. The prefix is
/// chosen to be unmistakable so accidental log-line collisions are nil.
/// Test: Indirect — exercised by the api/server `run_task` stream parser
/// and by the engine integration test path.
/// #196: Return the list of workflow phase names a given persona opts out of.
///
/// Why: `[hacker]`, `[vibe-coder]`, and `[novice]` were previously inert in the
/// prescriptive workflow because the engine ran every phase regardless of the
/// detected persona. Centralising the per-persona skip rules here keeps the
/// behaviour discoverable in one place and makes it trivial to extend with
/// new personas without touching the phase loop.
/// What: A static slice per persona. The default `engineer` persona returns
/// an empty slice (no phases skipped), preserving full-pipeline behaviour and
/// backward compatibility for tasks without any persona tag.
/// Test: `phases_to_skip_*` unit tests below assert each persona's skip set.
fn phases_to_skip(persona: &str) -> &'static [&'static str] {
    match persona {
        // Hacker: code-only. Skip research (heavyweight), plan (Opus is too
        // slow for throwaway scripts), QA (no test suite for one-off scripts),
        // and docs (no README for throwaway code). Fixes t06 latency: prior
        // skip set kept `plan` running on Opus (~120s) for ~269s total runtime.
        "hacker" => &["research", "plan", "qa", "docs"],
        // Vibe-coder: prototype-fast. Skip everything except code so the user
        // sees working output immediately. Plan is also skipped because the
        // request is "iterate", not "design first".
        "vibe-coder" => &["research", "plan", "qa", "docs"],
        // Novice: full pipeline (verbose output is handled by the persona
        // skill pack injected into the agent's prompt, not by skipping phases).
        "novice" => &[],
        // Engineer (default) and any unknown persona: full pipeline.
        _ => &[],
    }
}

fn emit_progress_event(
    name: &str,
    status: &str,
    elapsed_secs: f32,
    cost_usd: f32,
    note: Option<&str>,
) {
    // Legacy line — preserved for backwards compatibility with older parent
    // binaries that only know how to parse `__OMPM_PROGRESS__`.
    let event = serde_json::json!({
        "name": name,
        "status": status,
        "elapsed_secs": elapsed_secs,
        "cost_usd": cost_usd,
        "note": note,
    });
    eprintln!("__OMPM_PROGRESS__ {event}");

    // #192 Phase B: also emit a typed `Event::PhaseStarted` /
    // `Event::PhaseDone` on stderr (and the local in-process bus) so SSE
    // subscribers in the parent API server get phase transitions in real
    // time. `OPEN_MPM_RUN_ID` is set by the workflow harness; fall back to
    // an empty session id so the event is still visible (it just won't be
    // filtered by session).
    let session_id = std::env::var("OPEN_MPM_RUN_ID").unwrap_or_default();
    let phase = name.to_string();
    let typed = if status == "running" {
        crate::events::Event::PhaseStarted { session_id, phase }
    } else {
        crate::events::Event::PhaseDone {
            session_id,
            phase,
            status: status.to_string(),
        }
    };
    crate::events::emit(typed);
}

/// Recursively copy a directory tree from `src` to `dst`, refusing symlinks.
///
/// Why: Kept as a utility for callers that still need directory copies (the
/// cwd-fallback in the code phase was removed, but other paths may adopt
/// this helper in the future). CRIT-3 (#92): the previous implementation
/// followed symlinks, which let an attacker with write access to a source
/// tree redirect the copy into sensitive directories. We now query
/// `symlink_metadata` (does NOT follow symlinks) and skip any symlink entry
/// with a warning.
/// What: Creates `dst` if absent, then copies every regular file and
/// subdirectory recursively. Symlinks are skipped and logged. Existing
/// Discover the generated project root inside `out_dir`.
///
/// Why (#140): The code phase (especially claude-code engineers) often
/// writes the generated project into a subdirectory of `out_dir`
/// (e.g. `out/l4-bakeoff-.../doc_pipeline/` rather than directly into
/// `out/l4-bakeoff-.../`). When QA runs pytest against `out_dir`, pytest
/// finds no `tests/` and exits with code 5 ("no tests collected").
/// #160: Max age of a stray `assignments.json` at the project root that we
/// will still treat as belonging to the just-finished plan phase. Anything
/// older is presumed to be a stale leftover from a prior run (possibly
/// abandoned) and is NOT relocated — silently moving an old file could mask
/// real plan failures by making the code phase see outdated waves.
const POST_PLAN_RELOCATION_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// #123: Returns true if `agent_name` is configured with `runner = "claude-code"`.
///
/// Why: The claude CLI subprocess sometimes anchors relative `write_file`
/// calls to the git repository root rather than `RunContext::working_dir`.
/// We only trigger the post-code reconciliation for runners with this known
/// behavior — for subprocess/inline runners the files always land in
/// `out_dir` and we shouldn't disturb anything at the project root.
/// What: Loads the agent TOML; on any failure returns false (best effort).
/// Test: Indirectly via `qa_receives_correct_path_for_claude_code_runner`.
fn agent_uses_claude_code(agent_name: &str) -> bool {
    AgentConfig::by_name(agent_name)
        .map(|c| c.agent.runner == crate::agents::RunnerKind::ClaudeCode)
        .unwrap_or(false)
}

/// #123: After the code phase, move files that the code agent (claude-code
/// runner) wrote to the project root into `out_dir`.
///
/// Why: Even with `current_dir(out_dir)` and `--add-dir out_dir`, the claude
/// CLI occasionally anchors relative `write_file` paths to the git repo root
/// (its inherited CWD). The QA agent runs pytest against `out_dir` (or the
/// `{{project_dir}}` discovered inside it), so files at the project root are
/// invisible to QA and produce false-negative test failures. This routine
/// reads `assignments.json` from `out_dir`, and for each listed path that is
/// (a) missing in `out_dir` and (b) present at the project root with a recent
/// mtime, moves it into `out_dir`.
/// What: Best-effort. Skips silently when no `assignments.json` is present
/// (legacy monolithic path), when the project root cannot be read, or when
/// individual moves fail. Recency check uses the same 10-minute window as
/// `POST_PLAN_RELOCATION_MAX_AGE` to avoid picking up stale leftovers.
/// Test: `post_code_reconciles_files_from_project_root` exercises the move.
async fn reconcile_code_outputs_from_project_root(
    out_dir: &std::path::Path,
) -> std::io::Result<()> {
    let project_root = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!(error = %e, "reconcile_code_outputs: cannot read CWD");
            return Ok(());
        }
    };
    reconcile_code_outputs_from(&project_root, out_dir).await
}

/// #222: Reconcile code outputs against an explicit code target.
///
/// Why: When `--project-dir` is set, generated files belong in `code_dir`,
/// not `out_dir`. The plan-agent's `assignments.json` still lives in
/// `out_dir` (artifacts), so reconciliation has to read the manifest from
/// one path and check/move files into another.
/// What: Loads `assignments.json` from `assignments_dir`; for each listed
/// file, if it's missing in `code_target` but present at the git project
/// root (CWD) with a recent mtime, moves it into `code_target`. When
/// `assignments_dir == code_target` (legacy mode) delegates to the
/// pre-#222 `reconcile_code_outputs_from_project_root`.
/// Test: Indirect — covered by the existing
/// `post_code_reconciles_files_from_project_root` path when paths align;
/// the divergent path is exercised by manual smoke for `--project-dir .`.
async fn reconcile_code_outputs_against(
    assignments_dir: &std::path::Path,
    code_target: &std::path::Path,
) -> std::io::Result<()> {
    if assignments_dir == code_target {
        return reconcile_code_outputs_from_project_root(code_target).await;
    }
    let project_root = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!(error = %e, "reconcile_code_outputs_against: cannot read CWD");
            return Ok(());
        }
    };
    let assignments = match Assignments::load(assignments_dir) {
        Some(a) => a,
        None => {
            tracing::debug!(
                assignments_dir = %assignments_dir.display(),
                "reconcile_code_outputs_against: no assignments.json; skipping"
            );
            return Ok(());
        }
    };
    let mut moved = 0usize;
    for wave in &assignments.waves {
        for file in &wave.files {
            let dest = match safe_join(code_target, &file.path) {
                Some(p) => p,
                None => continue,
            };
            if tokio::fs::try_exists(&dest).await.unwrap_or(false) {
                continue;
            }
            let stray = project_root.join(&file.path);
            if !tokio::fs::try_exists(&stray).await.unwrap_or(false) {
                continue;
            }
            // When code_target effectively == project_root (e.g.
            // `--project-dir .`), the file is already where it belongs.
            if let (Ok(a), Ok(b)) = (std::fs::canonicalize(&stray), std::fs::canonicalize(&dest))
                && a == b
            {
                continue;
            }
            let meta = match tokio::fs::metadata(&stray).await {
                Ok(m) => m,
                Err(_) => continue,
            };
            let is_recent = meta
                .modified()
                .ok()
                .and_then(|mt| mt.elapsed().ok())
                .map(|age| age <= POST_PLAN_RELOCATION_MAX_AGE)
                .unwrap_or(false);
            if !is_recent {
                continue;
            }
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            if let Err(rename_err) = tokio::fs::rename(&stray, &dest).await {
                tracing::debug!(error = %rename_err, "rename failed; copy+delete");
                tokio::fs::copy(&stray, &dest).await?;
                let _ = tokio::fs::remove_file(&stray).await;
            }
            tracing::warn!(
                from = %stray.display(),
                to = %dest.display(),
                "code phase wrote file to git root — relocated to code_dir"
            );
            moved += 1;
        }
    }
    if moved > 0 {
        tracing::info!(
            moved,
            "reconcile_code_outputs_against: relocated files into code_dir"
        );
    }
    Ok(())
}

/// Testable inner routine for `reconcile_code_outputs_from_project_root`.
///
/// Why: Lets unit tests pass an explicit `project_root` instead of mutating
/// the process-wide `std::env::current_dir` (unsafe in multi-threaded test
/// runners, mirrors the pattern used by `relocate_plan_outputs_from`).
/// What: Reads `out_dir/assignments.json`; for each file listed in any wave,
/// if the path is missing in `out_dir` but present at `project_root` and
/// modified within the last 10 minutes, rename (or copy+delete) it to
/// `out_dir/<rel>`. Logs per-file actions at WARN.
/// Test: `post_code_reconciles_files_from_project_root` below.
async fn reconcile_code_outputs_from(
    project_root: &std::path::Path,
    out_dir: &std::path::Path,
) -> std::io::Result<()> {
    let assignments = match Assignments::load(out_dir) {
        Some(a) => a,
        None => {
            tracing::debug!(
                out_dir = %out_dir.display(),
                "reconcile_code_outputs: no assignments.json; skipping"
            );
            return Ok(());
        }
    };

    let mut moved = 0usize;
    let mut skipped_recent = 0usize;
    for wave in &assignments.waves {
        for file in &wave.files {
            // #114: Refuse to act on any path that escapes out_dir, even if
            // validate_file_path was bypassed. safe_join returns None for
            // any traversal attempt.
            let dest = match safe_join(out_dir, &file.path) {
                Some(p) => p,
                None => {
                    tracing::warn!(
                        path = %file.path,
                        "reconcile_code_outputs: refusing to act on unsafe path"
                    );
                    continue;
                }
            };
            if tokio::fs::try_exists(&dest).await.unwrap_or(false) {
                // Happy path — claude-code wrote it where we expected.
                continue;
            }

            // Misroute candidate: same relative path under the project root.
            let stray = project_root.join(&file.path);
            if !tokio::fs::try_exists(&stray).await.unwrap_or(false) {
                continue;
            }

            // Recency gate: only move if mtime is recent enough to plausibly
            // belong to the just-finished code phase.
            let meta = match tokio::fs::metadata(&stray).await {
                Ok(m) => m,
                Err(e) => {
                    tracing::debug!(error = %e, path = %stray.display(),
                        "reconcile_code_outputs: stat failed");
                    continue;
                }
            };
            let is_recent = meta
                .modified()
                .ok()
                .and_then(|mt| mt.elapsed().ok())
                .map(|age| age <= POST_PLAN_RELOCATION_MAX_AGE)
                .unwrap_or(false);
            if !is_recent {
                skipped_recent += 1;
                continue;
            }

            // Ensure parent directory exists in out_dir before move.
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }

            if let Err(rename_err) = tokio::fs::rename(&stray, &dest).await {
                tracing::debug!(
                    error = %rename_err,
                    "reconcile_code_outputs: rename failed; falling back to copy+delete"
                );
                tokio::fs::copy(&stray, &dest).await?;
                if let Err(e) = tokio::fs::remove_file(&stray).await {
                    tracing::debug!(
                        error = %e,
                        path = %stray.display(),
                        "reconcile_code_outputs: could not remove stray file after copy"
                    );
                }
            }

            tracing::warn!(
                from = %stray.display(),
                to = %dest.display(),
                "code phase wrote file to git root instead of out_dir — relocated to out_dir"
            );
            moved += 1;
        }
    }

    if moved > 0 || skipped_recent > 0 {
        tracing::info!(
            moved = moved,
            skipped_too_old = skipped_recent,
            "reconcile_code_outputs: post-code reconciliation summary"
        );
    }
    Ok(())
}

/// Why: The plan-agent's claude CLI subprocess writes relative paths
/// (e.g. `write_file("assignments.json", ...)`) against its inherited CWD,
/// which is the git repository root — NOT `RunContext::working_dir`. When
/// that happens, `assignments.json` and `stubs/` land at the project root
/// instead of `out_dir`, and the code phase's wave-loop check (which only
/// looks in `out_dir`) silently falls back to legacy monolithic mode. This
/// defense-in-depth step detects that misroute after the plan phase and
/// relocates the outputs so the wave loop triggers correctly.
/// What: If `out_dir/assignments.json` is already present, does nothing.
/// Otherwise, if `{CWD}/assignments.json` exists AND was modified within
/// the last 10 minutes, moves it (rename, fallback to copy+delete across
/// devices) to `out_dir/assignments.json` and WARN-logs. Also relocates
/// `{CWD}/stubs/` to `out_dir/stubs/` when present.
/// Test: `post_plan_relocates_assignments_json_from_git_root` below.
async fn relocate_plan_outputs_from_project_root(out_dir: &std::path::Path) -> std::io::Result<()> {
    let project_root = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!(error = %e, "relocate_plan_outputs: cannot read CWD");
            return Ok(());
        }
    };
    relocate_plan_outputs_from(&project_root, out_dir).await
}

/// Testable inner routine: same behavior as
/// `relocate_plan_outputs_from_project_root` but with the project root
/// explicitly passed in so tests can supply a simulated CWD without
/// mutating the process-wide `std::env::current_dir` (which is unsafe in
/// multi-threaded test runners).
async fn relocate_plan_outputs_from(
    project_root: &std::path::Path,
    out_dir: &std::path::Path,
) -> std::io::Result<()> {
    let out_asg = out_dir.join("assignments.json");
    if tokio::fs::try_exists(&out_asg).await.unwrap_or(false) {
        // Happy path — plan-agent wrote it where we expected. Nothing to do.
        return Ok(());
    }

    let root_asg = project_root.join("assignments.json");
    let root_asg_exists = tokio::fs::try_exists(&root_asg).await.unwrap_or(false);
    if !root_asg_exists {
        // Nothing misrouted. plan-agent just didn't produce assignments.json
        // at all — the code phase's existing "legacy monolithic" fallback
        // path will log that decision clearly.
        return Ok(());
    }

    // Recency check: only relocate if the file was touched recently enough
    // to plausibly belong to the plan phase we just finished.
    let meta = tokio::fs::metadata(&root_asg).await?;
    let is_recent = meta
        .modified()
        .ok()
        .and_then(|mt| mt.elapsed().ok())
        .map(|age| age <= POST_PLAN_RELOCATION_MAX_AGE)
        .unwrap_or(false);
    if !is_recent {
        tracing::debug!(
            path = %root_asg.display(),
            "found assignments.json at project root but it's too old to be from this plan phase; ignoring"
        );
        return Ok(());
    }

    // Ensure out_dir exists before the move.
    tokio::fs::create_dir_all(out_dir).await?;

    // Try rename first (atomic on same filesystem); fall back to copy+delete.
    if let Err(rename_err) = tokio::fs::rename(&root_asg, &out_asg).await {
        tracing::debug!(error = %rename_err, "rename failed, falling back to copy+delete");
        tokio::fs::copy(&root_asg, &out_asg).await?;
        // Best-effort delete; if we can't remove it we still succeeded in
        // seeding out_dir, which is what the code phase needs.
        if let Err(e) = tokio::fs::remove_file(&root_asg).await {
            tracing::debug!(error = %e, "could not remove stray assignments.json at project root");
        }
    }

    tracing::warn!(
        from = %root_asg.display(),
        to = %out_asg.display(),
        "plan phase wrote assignments.json to git root instead of out_dir — relocated to out_dir"
    );

    // Also relocate stubs/ if the plan-agent put it at the project root.
    let root_stubs = project_root.join("stubs");
    let out_stubs = out_dir.join("stubs");
    if tokio::fs::try_exists(&root_stubs).await.unwrap_or(false)
        && !tokio::fs::try_exists(&out_stubs).await.unwrap_or(false)
    {
        // Only relocate if recent, using directory mtime as a proxy.
        let stubs_meta = tokio::fs::metadata(&root_stubs).await?;
        let stubs_recent = stubs_meta
            .modified()
            .ok()
            .and_then(|mt| mt.elapsed().ok())
            .map(|age| age <= POST_PLAN_RELOCATION_MAX_AGE)
            .unwrap_or(false);
        if stubs_recent {
            if let Err(e) = tokio::fs::rename(&root_stubs, &out_stubs).await {
                // Cross-device or non-empty-target; try recursive copy.
                tracing::debug!(error = %e, "stubs rename failed, copying recursively");
                if let Err(copy_err) = copy_dir_all(&root_stubs, &out_stubs) {
                    tracing::warn!(error = %copy_err, "failed to copy stubs/ from project root");
                } else {
                    let _ = std::fs::remove_dir_all(&root_stubs);
                }
            }
            tracing::warn!(
                from = %root_stubs.display(),
                to = %out_stubs.display(),
                "plan phase wrote stubs/ to git root instead of out_dir — relocated to out_dir"
            );
        }
    }

    Ok(())
}

/// What: Returns `out_dir` itself if it directly contains `pyproject.toml`,
/// otherwise returns the first immediate child directory (excluding
/// `.venv`, `__pycache__`, dotfiles) that contains `pyproject.toml`.
/// Falls back to `out_dir` if no project root is found so callers always
/// get a usable path.
/// Test: `discover_project_dir_finds_subdirectory` and
/// `discover_project_dir_falls_back_to_out_dir` below.
/// Build a short summary string for a discovered skill (#173).
///
/// Why: The plan-agent only needs a one-liner per candidate skill so it can
/// decide whether to load the full body. Pulling the summary here keeps the
/// engine's prompt assembly bounded (~200 chars/skill * N).
/// What: Prefers the frontmatter `description` when non-empty; otherwise
/// reads the file body, strips frontmatter, and returns up to 200 chars of
/// the body trimmed and collapsed onto a single line. Returns "(no
/// description)" on read failure — never panics.
/// Test: `discover_skills_for_task_extracts_python_fastapi_tags` covers the
/// description path; the body-fallback path is exercised when the registry
/// has skills with empty descriptions.
fn skill_summary_for(meta: &crate::skills::registry::SkillMeta) -> String {
    let trimmed = meta.description.trim();
    if !trimmed.is_empty() {
        return truncate_summary(trimmed);
    }
    // Fallback: read the file body and synthesize a summary. Use blocking
    // read because this runs at most once per discovered skill at workflow
    // startup; the registry is small (<100 skills typical).
    match std::fs::read_to_string(&meta.source_path) {
        Ok(raw) => {
            let body = strip_skill_frontmatter(&raw);
            truncate_summary(body.trim())
        }
        Err(e) => {
            tracing::warn!(
                name = %meta.name,
                path = %meta.source_path.display(),
                error = %e,
                "skill discovery: failed to read body for summary; using placeholder"
            );
            "(no description)".to_string()
        }
    }
}

/// Collapse whitespace and clip to ~200 chars for a single-line summary.
fn truncate_summary(s: &str) -> String {
    let collapsed: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= 200 {
        collapsed
    } else {
        let mut out: String = collapsed.chars().take(200).collect();
        out.push('…');
        out
    }
}

/// Strip a leading YAML frontmatter block (`---\n...\n---\n`) from a Markdown
/// skill file. Mirrors the helper in `skills::mod` but is duplicated here to
/// avoid leaking a private module item across the crate boundary.
fn strip_skill_frontmatter(raw: &str) -> &str {
    if !raw.starts_with("---") {
        return raw;
    }
    let after_first = match raw.find("---\n") {
        Some(p) => &raw[p + 4..],
        None => return raw,
    };
    match after_first.find("\n---\n") {
        Some(p) => &after_first[p + 5..],
        None => raw,
    }
}

/// Parsed QA agent envelope (claude-mpm parity).
///
/// Why: When the QA agent emits structured output we can gate workflow
/// advancement on `status` and capture exact `passed`/`failed` counts for
/// perf records. Falling back to opaque text keeps every existing workflow
/// working unchanged.
/// What: Best-effort JSON extraction — accepts a bare JSON document, a
/// fenced ```json``` block, or a JSON object embedded anywhere in free text.
/// Test: `qa_envelope_parses_status_and_counts`,
/// `qa_envelope_returns_none_for_free_text`.
#[derive(Debug, Clone)]
pub(crate) struct QaEnvelope {
    pub status: QaStatus,
    pub passed: Option<u64>,
    pub failed: Option<u64>,
    pub details: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QaStatus {
    Pass,
    Fail,
}

pub(crate) fn parse_qa_envelope(raw: &str) -> Option<QaEnvelope> {
    let candidates = extract_json_candidates(raw);
    for candidate in candidates {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&candidate) {
            let status = value.get("status").and_then(|s| s.as_str())?;
            let status = match status.trim().to_ascii_lowercase().as_str() {
                "pass" | "passed" | "ok" | "success" => QaStatus::Pass,
                "fail" | "failed" | "error" => QaStatus::Fail,
                _ => return None,
            };
            let passed = value.get("passed").and_then(|v| v.as_u64());
            let failed = value.get("failed").and_then(|v| v.as_u64());
            // Prefer `details`; fall back to joined `errors`; then `summary`.
            let details = value
                .get("details")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| {
                    value.get("errors").and_then(|v| v.as_array()).map(|arr| {
                        arr.iter()
                            .filter_map(|e| e.as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                })
                .or_else(|| {
                    value
                        .get("summary")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                });
            return Some(QaEnvelope {
                status,
                passed,
                failed,
                details,
            });
        }
    }
    None
}

/// Extract JSON candidate substrings from a raw QA agent output blob.
///
/// Why: Agents may emit a bare JSON document, a fenced ```json``` block, or
/// JSON embedded in markdown narration. We try each shape in order so the
/// most disciplined output wins, but free-text outputs degrade gracefully
/// to `None`.
/// What: Returns up to three candidates: the trimmed input, the contents of
/// the first ```json``` fence, and the substring from the first `{` to the
/// last `}`.
/// Test: `qa_envelope_parses_status_and_counts`.
fn extract_json_candidates(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let trimmed = raw.trim();
    if !trimmed.is_empty() {
        out.push(trimmed.to_string());
    }
    // Fenced ```json``` block.
    if let Some(start) = raw.find("```json") {
        let after = &raw[start + "```json".len()..];
        if let Some(end) = after.find("```") {
            let body = after[..end].trim();
            if !body.is_empty() {
                out.push(body.to_string());
            }
        }
    }
    // First-{ to last-} embedded scan as a last resort.
    if let (Some(s), Some(e)) = (raw.find('{'), raw.rfind('}'))
        && e > s
    {
        out.push(raw[s..=e].to_string());
    }
    out
}

fn discover_project_dir(out_dir: &std::path::Path) -> Option<PathBuf> {
    // Direct hit: pyproject.toml sits right in out_dir.
    if out_dir.join("pyproject.toml").is_file() {
        return Some(out_dir.to_path_buf());
    }
    // Scan one level of immediate children for a project marker.
    let entries = match std::fs::read_dir(out_dir) {
        Ok(e) => e,
        Err(_) => return Some(out_dir.to_path_buf()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        // Skip virtualenvs, caches, and hidden directories — they often
        // contain their own pyproject.toml (e.g. inside site-packages)
        // and would produce false positives.
        if name.starts_with('.') || name == "__pycache__" || name == "node_modules" {
            continue;
        }
        if path.join("pyproject.toml").is_file() {
            return Some(path);
        }
    }
    // No subdirectory project found — fall back to out_dir so {{project_dir}}
    // behaves identically to {{out_dir}} for the common case.
    Some(out_dir.to_path_buf())
}

/// files in `dst` are overwritten.
/// Test: Covered by code review — behavioral tests would require symlink
/// setup that's platform-specific.
#[allow(dead_code)]
fn copy_dir_all(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        // CRIT-3 (#92): `symlink_metadata` does NOT follow symlinks; this is
        // the only correct way to refuse to traverse them.
        let file_type = entry.path().symlink_metadata()?.file_type();
        if file_type.is_symlink() {
            tracing::warn!(
                path = ?entry.path(),
                "copy_dir_all: skipping symlink to avoid following arbitrary paths"
            );
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// #88: Run the per-file wave loop for the code phase.
///
/// Why: Replacing a monolithic code-agent invocation with one sub-agent per
/// file means each agent has a tight scope (one path, one stub, listed deps)
/// and the write_file tool can enforce single-file write authority. Each wave
/// runs sequentially so later waves see their dependencies on disk.
/// What: For each wave in order, for each file in the wave (sequential),
/// builds a focused prompt and calls `runner.run_with_context` with a
/// `RunContext` carrying the assigned file, a 20-turn cap, and `out_dir` as
/// the child's CWD. CRIT-1/MAJ-1 (#90, #93): this replaces unsafe
/// `std::env::set_var` threading and the cwd-fallback copy. MAJ-2 (#94): on
/// per-file failure, any already-completed files are preserved in the
/// returned error's context via the accumulated `combined` buffer being
/// logged before propagation.
/// Test: `wave_loop_runs_one_agent_per_file`.
async fn run_wave_loop(
    assignments: Assignments,
    phase: &PhaseDef,
    runner: Arc<dyn AgentRunner>,
    code_dir: &std::path::Path,
    artifacts_dir: &std::path::Path,
    progress: Option<Arc<crate::progress::ProgressReporter>>,
) -> anyhow::Result<AgentOutput> {
    // #222: `code_dir` is where source files are written; `artifacts_dir` is
    // where the plan-agent staged `assignments.json` and `stubs/`. In the
    // legacy default these point to the same directory.
    let out_dir = code_dir;
    let error_convention = assignments
        .error_convention
        .as_deref()
        .unwrap_or("exceptions")
        .to_string();

    let mut combined = String::new();
    let mut agg_usage = crate::perf::TokenUsage::default();
    let mut written_paths: Vec<String> = Vec::new();

    // #231: Source model-elevation config from the agent's TOML once, before
    // the wave loop dispatches files. Missing or unparseable TOML simply
    // disables elevation (None, None) — the same fail-soft posture used
    // elsewhere in this file.
    let (elevation_threshold, elevation_model) =
        match AgentConfig::by_name_async(&phase.agent).await {
            Ok(cfg) => (cfg.llm.elevation_threshold, cfg.llm.elevation_model.clone()),
            Err(_) => (None, None),
        };

    // #150: Pre-create empty package-marker files (e.g. `__init__.py`) before
    // the wave loop dispatches any agents. Why: engineer agents treat
    // trivially-empty placeholder files as not worth writing, but the
    // post-dispatch file-presence check then hard-errors. Pre-creating them
    // as empty files satisfies the check regardless of agent behavior.
    precreate_package_markers(&assignments, out_dir).await?;

    let wave_total = assignments.waves.len();
    for wave in &assignments.waves {
        info!(wave = wave.wave, files = wave.files.len(), "starting wave");
        // #149: Stream per-wave progress so the code phase doesn't appear
        // stuck during long sequential waves.
        let wave_started = std::time::Instant::now();
        if let Some(rep) = &progress {
            rep.wave_start(wave.wave as usize, wave_total, wave.files.len());
        }
        for file in &wave.files {
            // #114: Defense-in-depth — `Assignments::load` already validates
            // file paths, but a caller constructing an `Assignments` value
            // programmatically would bypass that. Re-check here so every
            // `out_dir.join(&file.path)` is guaranteed safe.
            if let Err(e) = Assignments::validate_file_path(&file.path) {
                return Err(anyhow::anyhow!(
                    "wave-loop: rejecting unsafe file path: {e}"
                ));
            }
            // #114: Belt-and-suspenders — even if `validate_file_path` accepts
            // the lexical form, canonicalize and confirm the resolved path
            // sits inside `out_dir`. Guards against any future bypass and
            // catches symlink-rewritten ancestors. WARN+skip rather than
            // hard-erroring so one bad path can't fail the whole wave.
            if safe_join(out_dir, &file.path).is_none() {
                tracing::warn!(
                    path = %file.path,
                    out_dir = %out_dir.display(),
                    "wave-loop: rejecting path that escapes out_dir; skipping file"
                );
                continue;
            }
            let max_lines = file.max_lines.unwrap_or(300);
            let deps = if file.depends_on.is_empty() {
                "none".to_string()
            } else {
                file.depends_on
                    .iter()
                    .map(|d| format!("`{d}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };

            // #222: When code_dir != artifacts_dir, stubs live in
            // artifacts_dir/stubs/ but the agent's CWD is code_dir. Pass an
            // ABSOLUTE stub path so the engineer can find it regardless of
            // its working directory.
            let stub_instruction = match &file.stub {
                Some(stub) => {
                    let abs_stub = artifacts_dir.join("stubs").join(stub);
                    format!(
                        "1. Read your stub: `{abs_stub}` (ABSOLUTE PATH) — implement it exactly, do not change signatures",
                        abs_stub = abs_stub.display()
                    )
                }
                None => format!(
                    "1. No stub for this file — implement based on purpose: {}",
                    file.purpose
                ),
            };
            // #166: Compute absolute path for the write step. The claude CLI
            // subprocess anchors relative `write_file` calls to the git
            // repository root, so a relative path like
            // `multi_repo_analyzer/pyproject.toml` would land at the repo
            // root instead of out_dir. We keep the relative `file.path` for
            // reading stubs and dependencies (those are relative to the
            // working_dir/out_dir), but instruct the agent to use the
            // absolute path for the write_file call.
            let abs_path = out_dir.join(&file.path);
            let abs_path_str = abs_path.to_string_lossy();
            let task = format!(
                "You are responsible for ONE file: `{path}`\n\
                 Purpose: {purpose}\n\
                 Max lines: {max_lines}\n\
                 Error convention: {error_convention}\n\n\
                 Steps:\n\
                 {stub_instruction}\n\
                 2. Read each dependency for context: {deps}\n\
                 3. Add a one-line intent comment above each function: # INTENT: <what it does, not how>\n\
                 4. Write full implementation to `{abs_path}` via write_file (ABSOLUTE PATH, not relative).\n\
                    IMPORTANT: Use the ABSOLUTE path `{abs_path}` when calling write_file.\n\
                    Do NOT use the relative path `{path}` for writing — it will land in the wrong directory.\n\
                 5. Run your tests if possible: the test file for this module (if it exists in stubs/)\n\
                 6. Call finish_task when done\n",
                path = file.path,
                abs_path = abs_path_str,
                purpose = file.purpose,
                max_lines = max_lines,
                error_convention = error_convention,
                stub_instruction = stub_instruction,
                deps = deps,
            );

            // CRIT-1 / MAJ-1 (#90, #93): Per-file overrides travel through
            // the runner trait as a `RunContext`; the runner applies them to
            // the child process only (never mutating the parent env). The
            // working_dir=out_dir ensures claude-code writes land directly in
            // out_dir instead of the parent's cwd.
            let ctx = RunContext {
                assigned_file: Some(std::path::PathBuf::from(&file.path)),
                max_turns_override: Some(40),
                working_dir: Some(out_dir.to_path_buf()),
                // #107: Thread the phase-level model override through each
                // per-file wave-loop invocation so the code phase's
                // `model` reaches the claude-code runner.
                model: phase.model.clone(),
            };

            let result = run_wave_file_with_retry(
                runner.as_ref(),
                &phase.agent,
                &task,
                &ctx,
                elevation_threshold,
                elevation_model.as_deref(),
            )
            .await;

            let output = match result {
                Ok(o) => o,
                Err(e) => {
                    // MAJ-2 (#94): Emit the partial combined output before
                    // propagating so operators can see which files succeeded.
                    tracing::warn!(
                        path = %file.path,
                        wave = wave.wave,
                        error = %e,
                        completed_files = written_paths.len(),
                        partial_chars = combined.len(),
                        "wave-loop: per-file agent failed; partial output preserved in logs"
                    );
                    if !combined.is_empty() {
                        tracing::info!(
                            partial_content = %combined,
                            "wave-loop: partial combined output at point of failure"
                        );
                    }
                    return Err(e.context(format!(
                        "wave-loop: per-file agent failed for {} (completed: {} files)",
                        file.path,
                        written_paths.len()
                    )));
                }
            };

            info!(
                path = %file.path,
                wave = wave.wave,
                prompt_tokens = output.usage.prompt_tokens,
                completion_tokens = output.usage.completion_tokens,
                "wave-loop file complete"
            );

            agg_usage.add(&output.usage);
            combined.push_str(&format!(
                "## File: {path} (wave {wave})\n\n",
                path = file.path,
                wave = wave.wave
            ));
            combined.push_str(&output.content);
            combined.push_str("\n\n");

            // #114: The agent must have written the file to out_dir. With
            // working_dir=out_dir set on the runner, cwd-fallback is no
            // longer necessary — either the file is there or the agent
            // misbehaved. Previously this was a warning; escalating to a
            // hard error prevents later waves from running against missing
            // dependencies (adopted from self-review patch 03).
            let dest = out_dir.join(&file.path);
            if dest.exists() {
                written_paths.push(file.path.clone());
            } else {
                return Err(anyhow::anyhow!(
                    "wave-loop: agent for '{}' completed successfully but did not write \
                     the assigned file to disk at '{}'. {} prior file(s) were written \
                     successfully. This is treated as a hard error because later waves \
                     may depend on this file.",
                    file.path,
                    dest.display(),
                    written_paths.len(),
                ));
            }
        }
        // #149: Stream wave-done after all files in this wave have been written.
        if let Some(rep) = &progress {
            rep.wave_done(wave.wave as usize, wave_total, wave_started.elapsed());
        }
    }

    // #228: Clean up plan-agent scaffolding stubs after code phase completes.
    // The stubs/ directory was used as implementation hints during the wave
    // loop. Leaving it on disk causes recursive test runners (e.g. `go test ./...`,
    // `pytest --rootdir`) to pick up unfilled stub functions and fail.
    let stubs_dir = artifacts_dir.join("stubs");
    if stubs_dir.exists() {
        match tokio::fs::remove_dir_all(&stubs_dir).await {
            Ok(_) => tracing::info!(
                path = %stubs_dir.display(),
                "wave-loop: removed stubs scaffolding dir after code phase"
            ),
            Err(e) => tracing::warn!(
                path = %stubs_dir.display(),
                error = %e,
                "wave-loop: could not remove stubs dir (non-fatal)"
            ),
        }
    }

    let summary = format!(
        "wave-loop complete: {} files across {} waves ({} written)",
        assignments
            .waves
            .iter()
            .map(|w| w.files.len())
            .sum::<usize>(),
        assignments.waves.len(),
        written_paths.len()
    );

    Ok(AgentOutput {
        content: combined,
        summary: Some(summary),
        usage: agg_usage,
    })
}

/// #206: Classify an error as retryable (transient API/network fault) or fatal.
///
/// Why: Anthropic and OpenRouter return transient 5xx / 429 errors during
/// brief overload windows that resolve within seconds. Aborting an entire
/// workflow run on such an error discards all already-completed files and
/// forces the operator to restart from scratch. Distinguishing transient
/// from fatal errors lets the wave loop retry safely without masking real
/// failures (bad request, auth error, missing config).
///
/// What: Inspects the stringified error for well-known transient signal
/// phrases. Any HTTP 4xx that is NOT 429 is NOT retryable -- it would fail
/// identically on retry. Fatal logic errors (empty output, missing TOML)
/// are also non-retryable.
///
/// Test: `is_retryable_classifies_5xx`, `is_retryable_rejects_4xx`.
pub(crate) fn is_retryable(err: &anyhow::Error) -> bool {
    let msg = err.to_string().to_lowercase();
    // Explicit HTTP status codes embedded in error strings.
    if msg.contains("status 500")
        || msg.contains("status 502")
        || msg.contains("status 503")
        || msg.contains("status 529")
        || msg.contains("status 429")
        || msg.contains("http 500")
        || msg.contains("http 502")
        || msg.contains("http 503")
        || msg.contains("http 529")
        || msg.contains("http 429")
        || msg.contains("error 500")
        || msg.contains("error 502")
        || msg.contains("error 503")
        || msg.contains("error 529")
        || msg.contains("error 429")
    {
        return true;
    }
    // Textual signals from Anthropic / OpenRouter error bodies.
    msg.contains("internal server error")
        || msg.contains("overloaded")
        || msg.contains("service unavailable")
        || msg.contains("bad gateway")
        || msg.contains("too many requests")
        || msg.contains("rate limit")
        || msg.contains("timeout")
        || msg.contains("timed out")
        || msg.contains("connection reset")
        || msg.contains("connection refused")
        || msg.contains("broken pipe")
}

/// #206: Invoke a single per-file wave agent with exponential backoff retry.
///
/// Why: Transient Anthropic 5xx / 429 errors during a wave cause the entire
/// workflow to abort, discarding completed files. Retrying with backoff gives
/// the API time to recover without operator intervention.
///
/// What: Calls `runner.run_with_context` up to `MAX_WAVE_RETRIES + 1` times.
/// On a retryable error it waits `BASE_DELAY_MS * 2^attempt` ms before the
/// next attempt and logs a WARN line. On a non-retryable (fatal) error it
/// returns immediately. After all retries are exhausted it returns the last
/// error.
///
/// The retry is placed at the *per-file agent dispatch* level rather than
/// inside the LLM client so it covers all runner paths (subprocess,
/// in-process, claude-code) uniformly and avoids billing surprises from
/// partial LLM responses being retried at the HTTP level.
///
/// Test: `wave_loop_retries_on_transient_error`,
///       `wave_loop_does_not_retry_fatal_error`.
async fn run_wave_file_with_retry(
    runner: &dyn AgentRunner,
    agent_name: &str,
    task: &str,
    ctx: &crate::tools::traits::RunContext,
    elevation_threshold: Option<u32>,
    elevation_model: Option<&str>,
) -> anyhow::Result<AgentOutput> {
    const MAX_WAVE_RETRIES: u32 = 2;
    const BASE_DELAY_MS: u64 = 2_000;

    let mut last_err: Option<anyhow::Error> = None;
    let mut total_attempts: u32 = 0;
    for attempt in 0..=MAX_WAVE_RETRIES {
        total_attempts = attempt + 1;
        match runner.run_with_context(agent_name, task, ctx).await {
            Ok(output) => return Ok(output),
            Err(e) if is_retryable(&e) && attempt < MAX_WAVE_RETRIES => {
                let delay_ms = BASE_DELAY_MS * (1u64 << attempt);
                tracing::warn!(
                    attempt = attempt + 1,
                    max_retries = MAX_WAVE_RETRIES,
                    delay_ms,
                    agent = %agent_name,
                    error = %e,
                    "wave-loop: transient error, retrying after backoff"
                );
                tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                last_err = Some(e);
            }
            Err(e) => {
                last_err = Some(e);
                break;
            }
        }
    }

    // #231: Model elevation — after exhausting normal retries on the base
    // model, optionally make ONE more attempt on the elevation model. The
    // threshold is the number of failures required to trigger elevation;
    // because we always run up to MAX_WAVE_RETRIES + 1 attempts on the base
    // model, any threshold <= total_attempts triggers when both fields are set.
    if let (Some(threshold), Some(elev_model)) = (elevation_threshold, elevation_model)
        && total_attempts >= threshold
    {
        tracing::info!(
            agent = %agent_name,
            from_model = ?ctx.model,
            to_model = %elev_model,
            attempts = total_attempts,
            threshold,
            "wave-loop: model elevation triggered after repeated failures"
        );
        let mut elevated_ctx = ctx.clone();
        elevated_ctx.model = Some(elev_model.to_string());
        return runner
            .run_with_context(agent_name, task, &elevated_ctx)
            .await;
    }

    // No elevation configured (or threshold not met) -- return the last error.
    Err(last_err.expect("retry loop exited without capturing an error"))
}

/// #150: Decide whether a wave-loop file assignment should be pre-created as
/// an empty file before agent dispatch.
///
/// Why: Engineer agents often skip "trivial" placeholder files like Python
/// package markers (`__init__.py`). The wave loop's post-dispatch presence
/// check then hard-errors, failing the entire phase. Pre-creating these
/// files prevents the false negative without changing agent behavior.
/// What: Returns true when the filename is `__init__.py`, or when the
/// assignment has no stub AND an empty/whitespace-only purpose (signaling
/// the plan-agent itself treated the file as a placeholder).
/// Test: `should_precreate_detects_init_py`, `should_precreate_detects_empty_stub_and_purpose`.
fn should_precreate(file: &crate::workflow::config::FileAssignment) -> bool {
    let name = std::path::Path::new(&file.path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if name == "__init__.py" {
        return true;
    }
    // Plan-agent placeholder pattern: `stub: null` with empty purpose.
    if file.stub.is_none() && file.purpose.trim().is_empty() {
        return true;
    }
    false
}

/// #150: Pre-create empty package-marker files referenced by the assignments
/// plan so the wave loop's post-dispatch presence check passes regardless of
/// whether the engineer agent chose to write them.
///
/// Why: See `should_precreate`. The hard-error on missing files was observed
/// in L2 bake-off build #166 when an agent skipped an empty `__init__.py`.
/// What: Iterates all wave files; for each one `should_precreate` accepts,
/// creates parent directories and writes an empty file if it does not yet
/// exist. Does not overwrite existing content.
/// Test: `precreate_package_markers_creates_init_py`.
async fn precreate_package_markers(
    assignments: &Assignments,
    out_dir: &std::path::Path,
) -> anyhow::Result<()> {
    for wave in &assignments.waves {
        for file in &wave.files {
            if !should_precreate(file) {
                continue;
            }
            // Defense-in-depth: reject unsafe paths here too, though
            // `Assignments::load` + `run_wave_loop` already validated.
            if Assignments::validate_file_path(&file.path).is_err() {
                continue;
            }
            let full_path = out_dir.join(&file.path);
            if full_path.exists() {
                continue;
            }
            if let Some(parent) = full_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&full_path, b"").await?;
            tracing::debug!(
                path = %file.path,
                "wave-loop: pre-created empty package marker"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;

    /// Why: Fix 2 — QA envelope parser must extract status + counts from a
    /// fenced ```json``` block, a bare JSON document, AND a JSON object
    /// embedded in surrounding markdown. Counts must round-trip.
    /// What: Three parse shapes asserted against a single canonical envelope.
    #[test]
    fn qa_envelope_parses_status_and_counts() {
        // Bare JSON
        let env = parse_qa_envelope(r#"{"status":"pass","passed":42,"failed":0,"summary":"ok"}"#)
            .expect("parse bare json");
        assert_eq!(env.status, QaStatus::Pass);
        assert_eq!(env.passed, Some(42));
        assert_eq!(env.failed, Some(0));

        // Fenced
        let env = parse_qa_envelope(
            "Here is the result:\n```json\n{\"status\":\"fail\",\"passed\":3,\"failed\":2,\"errors\":[\"e1\",\"e2\"]}\n```\n",
        )
        .expect("parse fenced json");
        assert_eq!(env.status, QaStatus::Fail);
        assert_eq!(env.passed, Some(3));
        assert_eq!(env.failed, Some(2));
        assert!(env.details.unwrap().contains("e1"));

        // Embedded
        let env = parse_qa_envelope(
            "I ran the suite. Result: {\"status\":\"fail\",\"failed\":1,\"details\":\"boom\"} done.",
        )
        .expect("parse embedded json");
        assert_eq!(env.status, QaStatus::Fail);
        assert_eq!(env.failed, Some(1));
        assert_eq!(env.details.as_deref(), Some("boom"));
    }

    /// Why: Fix 2 backward compatibility — free-text QA output must NOT
    /// produce a parsed envelope, so the workflow continues exactly as
    /// before.
    #[test]
    fn qa_envelope_returns_none_for_free_text() {
        assert!(parse_qa_envelope("All tests passed!").is_none());
        assert!(parse_qa_envelope("35/35 passed").is_none());
        assert!(parse_qa_envelope("").is_none());
        // JSON without `status` is also None (we require it).
        assert!(parse_qa_envelope(r#"{"passed":5}"#).is_none());
    }

    /// #196: `engineer` (default) gets the full pipeline — no phases skipped.
    #[test]
    fn phases_to_skip_engineer_is_empty() {
        assert_eq!(phases_to_skip("engineer"), &[] as &[&str]);
    }

    /// #196: Tasks without any persona tag fall through to `engineer`,
    /// preserving backward-compatible behaviour. We assert the unknown-arm
    /// path returns an empty slice so unknown personas don't accidentally
    /// drop phases.
    #[test]
    fn phases_to_skip_unknown_persona_is_empty() {
        assert_eq!(phases_to_skip(""), &[] as &[&str]);
        assert_eq!(phases_to_skip("rogue-persona"), &[] as &[&str]);
    }

    /// #196 + t06 fix: Hacker persona is code-only — research/plan/qa/docs
    /// are skipped. Plan runs on Opus (~120s) which is too heavyweight for
    /// throwaway scripts; skipping it brings hacker latency from ~269s to
    /// roughly the code+observe runtime.
    #[test]
    fn phases_to_skip_hacker() {
        let s = phases_to_skip("hacker");
        assert!(s.contains(&"research"), "hacker must skip research: {s:?}");
        assert!(s.contains(&"plan"), "hacker must skip plan: {s:?}");
        assert!(s.contains(&"qa"), "hacker must skip qa: {s:?}");
        assert!(s.contains(&"docs"), "hacker must skip docs: {s:?}");
        // The code phase MUST run for the hacker persona — that's the whole point.
        assert!(!s.contains(&"code"), "hacker must NOT skip code: {s:?}");
    }

    /// #196: Vibe-coder skips everything except code (and observe, which is
    /// always run if defined — observe is reporting, not gating).
    #[test]
    fn phases_to_skip_vibe_coder() {
        let s = phases_to_skip("vibe-coder");
        assert!(s.contains(&"research"));
        assert!(s.contains(&"plan"));
        assert!(s.contains(&"qa"));
        assert!(s.contains(&"docs"));
        assert!(!s.contains(&"code"), "vibe-coder must NOT skip code: {s:?}");
    }

    /// #196: Novice gets the full pipeline. Verbosity is delivered via the
    /// persona skill pack injected into the agent prompt, not by skipping
    /// phases. (Skipping QA for a learner would be actively harmful.)
    #[test]
    fn phases_to_skip_novice_is_empty() {
        assert_eq!(phases_to_skip("novice"), &[] as &[&str]);
    }

    /// #140: When out_dir itself contains pyproject.toml (the simple case
    /// where the engineer writes files directly into out_dir), discovery
    /// should return out_dir unchanged. Existing QA behavior must be
    /// preserved for this pattern.
    #[test]
    fn discover_project_dir_returns_out_dir_when_pyproject_at_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("pyproject.toml"), b"[project]\nname='x'\n").unwrap();
        let discovered = discover_project_dir(root).unwrap();
        assert_eq!(discovered, root);
    }

    /// #140: The primary bug scenario — engineer writes the project into
    /// `out_dir/task_board/` with pyproject.toml one level down.
    /// Discovery must return the subdirectory so pytest runs where
    /// tests/ actually lives.
    #[test]
    fn discover_project_dir_finds_subdirectory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let project = root.join("task_board");
        std::fs::create_dir(&project).unwrap();
        std::fs::write(project.join("pyproject.toml"), b"[project]\nname='x'\n").unwrap();
        std::fs::create_dir(project.join("tests")).unwrap();
        let discovered = discover_project_dir(root).unwrap();
        assert_eq!(discovered, project);
    }

    /// #140: Discovery must ignore `.venv` and hidden directories that
    /// routinely contain their own pyproject.toml inside site-packages.
    /// Without this guard, a stale virtualenv could hijack detection.
    #[test]
    fn discover_project_dir_ignores_venv_and_hidden_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Create a .venv with a dummy pyproject.toml — must be skipped.
        let venv = root.join(".venv");
        std::fs::create_dir(&venv).unwrap();
        std::fs::write(venv.join("pyproject.toml"), b"[project]\nname='venv'\n").unwrap();
        // Fallback should be out_dir itself since no real project exists.
        let discovered = discover_project_dir(root).unwrap();
        assert_eq!(discovered, root);
    }

    /// #140: When out_dir has no pyproject.toml anywhere, discovery falls
    /// back to out_dir so {{project_dir}} templates remain functional.
    #[test]
    fn discover_project_dir_falls_back_to_out_dir_when_no_project() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir(root.join("src")).unwrap();
        let discovered = discover_project_dir(root).unwrap();
        assert_eq!(discovered, root);
    }

    use anyhow::Result;
    use async_trait::async_trait;

    use crate::perf::TokenUsage;
    use crate::tools::traits::{AgentOutput, AgentRunner};

    /// Mock runner that:
    ///   - for the "code" agent, returns a content blob with two `## File:`
    ///     sections so the engine has something to extract;
    ///   - for the "qa" agent, snapshots the contents of `out_dir` at the
    ///     moment it is invoked — this is the heart of the #64 assertion:
    ///     files must exist BEFORE QA runs, not after the workflow ends.
    struct PhaseOrderMock {
        out_dir: PathBuf,
        qa_dir_snapshot: Arc<Mutex<Vec<PathBuf>>>,
    }

    #[async_trait]
    impl AgentRunner for PhaseOrderMock {
        async fn run(&self, agent_name: &str, _task: &str) -> Result<AgentOutput> {
            if agent_name == "qa-mock" {
                // Snapshot the out_dir so the test can assert what QA saw.
                let mut found = Vec::new();
                if self.out_dir.exists() {
                    let mut stack = vec![self.out_dir.clone()];
                    while let Some(p) = stack.pop() {
                        let mut rd = tokio::fs::read_dir(&p).await?;
                        while let Some(entry) = rd.next_entry().await? {
                            let path = entry.path();
                            if path.is_dir() {
                                stack.push(path);
                            } else {
                                found.push(path);
                            }
                        }
                    }
                }
                self.qa_dir_snapshot.lock().unwrap().extend(found);
                return Ok(AgentOutput {
                    content: "QA result: ok".to_string(),
                    summary: Some("QA ok".to_string()),
                    usage: TokenUsage::default(),
                });
            }

            if agent_name == "code-mock" {
                let content = "Here is the code.\n\n\
                    ## File: src/hello.py\n\
                    ```python\n\
                    def greet():\n    return \"hi\"\n\
                    ```\n\n\
                    ## File: tests/test_hello.py\n\
                    ```python\n\
                    from src.hello import greet\n\n\
                    def test_greet():\n    assert greet() == \"hi\"\n\
                    ```\n"
                    .to_string();
                return Ok(AgentOutput {
                    content,
                    summary: Some("code summary".into()),
                    usage: TokenUsage::default(),
                });
            }

            // Any other agent: return a harmless stub.
            Ok(AgentOutput {
                content: format!("stub output from {agent_name}"),
                summary: None,
                usage: TokenUsage::default(),
            })
        }
    }

    /// (#64) Files emitted by a `produces_files: true` phase must be on disk
    /// BEFORE the next phase runs — not after the workflow completes.
    /// We build a minimal two-phase workflow (code -> qa), wire a mock runner
    /// that snapshots the `out_dir` contents when QA is invoked, and assert
    /// both extracted files are visible from QA's perspective.
    #[tokio::test]
    async fn files_are_extracted_before_next_phase_runs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let out_dir = tmp.path().to_path_buf();

        // Write a minimal workflow JSON that exercises `produces_files`.
        let workflows_dir = tmp.path().join("workflows");
        tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
        let wf_path = workflows_dir.join("order-test.json");
        let wf_json = r#"{
            "name": "order-test",
            "description": "code -> qa order test",
            "phases": [
                {
                    "name": "code",
                    "agent": "code-mock",
                    "produces_files": true,
                    "context_template": "{{task}}"
                },
                {
                    "name": "qa",
                    "agent": "qa-mock",
                    "context_template": "verify {{out_dir}}"
                }
            ]
        }"#;
        tokio::fs::write(&wf_path, wf_json).await.unwrap();

        let snapshot: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
        let mock = Arc::new(PhaseOrderMock {
            out_dir: out_dir.clone(),
            qa_dir_snapshot: snapshot.clone(),
        });

        let engine = WorkflowEngine::new(mock, workflows_dir.clone());
        let ctx = engine
            .run("order-test", "do the thing".into(), Some(out_dir.clone()))
            .await
            .expect("workflow should complete");

        // Assert the snapshot QA captured contains BOTH files written by code.
        let snap = snapshot.lock().unwrap().clone();
        let hello = out_dir.join("src/hello.py");
        let test_hello = out_dir.join("tests/test_hello.py");
        assert!(
            snap.contains(&hello),
            "QA did not see src/hello.py; snapshot was {snap:?}"
        );
        assert!(
            snap.contains(&test_hello),
            "QA did not see tests/test_hello.py; snapshot was {snap:?}"
        );

        // And the engine recorded both phase outputs.
        assert!(ctx.phase_outputs.contains_key("code"));
        assert!(ctx.phase_outputs.contains_key("qa"));

        // Sanity: on-disk bodies match what the mock emitted.
        let hello_body = tokio::fs::read_to_string(&hello).await.unwrap();
        assert!(hello_body.contains("def greet():"));
    }

    /// A phase WITHOUT `produces_files` must not perform any extraction, even
    /// if its output contains `## File:` sections. This guards the opt-in
    /// semantics — only the code phase should touch disk.
    #[tokio::test]
    async fn phase_without_produces_files_does_not_extract() {
        let tmp = tempfile::tempdir().unwrap();
        let out_dir = tmp.path().to_path_buf();

        let workflows_dir = tmp.path().join("workflows");
        tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
        let wf_path = workflows_dir.join("no-extract.json");
        let wf_json = r#"{
            "name": "no-extract",
            "description": "no produces_files anywhere",
            "phases": [
                {
                    "name": "code",
                    "agent": "code-mock",
                    "context_template": "{{task}}"
                }
            ]
        }"#;
        tokio::fs::write(&wf_path, wf_json).await.unwrap();

        let snapshot: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
        let mock = Arc::new(PhaseOrderMock {
            out_dir: out_dir.clone(),
            qa_dir_snapshot: snapshot.clone(),
        });
        let engine = WorkflowEngine::new(mock, workflows_dir.clone());
        engine
            .run("no-extract", "x".into(), Some(out_dir.clone()))
            .await
            .expect("workflow ok");

        // No files should have been written.
        assert!(!out_dir.join("src/hello.py").exists());
        assert!(!out_dir.join("tests/test_hello.py").exists());
    }

    /// #153: Mock runner that captures the `working_dir` observed in
    /// `RunContext` on each `run_with_context` call. Used to assert the
    /// legacy monolithic code path passes `out_dir` as an *absolute* path
    /// to the runner (so subprocess-driven runners like claude-code write
    /// into out_dir instead of accidentally writing to the parent process
    /// CWD — i.e. the project root).
    struct WorkingDirCapture {
        working_dirs: Arc<Mutex<Vec<Option<PathBuf>>>>,
    }

    #[async_trait]
    impl AgentRunner for WorkingDirCapture {
        async fn run(&self, agent_name: &str, _task: &str) -> Result<AgentOutput> {
            // Fallback: record a `None` so tests can detect when the engine
            // dispatched through the plain `run` path (which loses working_dir).
            self.working_dirs.lock().unwrap().push(None);
            Ok(AgentOutput {
                content: format!("done {agent_name}"),
                summary: None,
                usage: TokenUsage::default(),
            })
        }

        async fn run_with_context(
            &self,
            agent_name: &str,
            _task: &str,
            ctx: &RunContext,
        ) -> Result<AgentOutput> {
            self.working_dirs
                .lock()
                .unwrap()
                .push(ctx.working_dir.clone());
            Ok(AgentOutput {
                content: format!("done {agent_name}"),
                summary: None,
                usage: TokenUsage::default(),
            })
        }
    }

    /// #153: The legacy monolithic code path (no `assignments.json`) must
    /// thread `out_dir` into `RunContext::working_dir` as an **absolute**
    /// path. If it passes a relative path (or `None`), subprocess-driven
    /// runners such as `ClaudeCodeAgentRunner` can write files into the
    /// parent process CWD (project root) instead of `out_dir`, which breaks
    /// `discover_project_dir` and causes QA to fail with "no tests found".
    ///
    /// We drive the engine with a CLI-shaped **relative** `out_dir`
    /// (`out/legacy-monolithic-test`), let `run_with_perf` create and
    /// canonicalize it, and assert the runner observed an absolute path.
    #[tokio::test]
    async fn legacy_monolithic_path_passes_absolute_working_dir() {
        // Drive a relative out_dir path, mirroring how the CLI
        // (`--out-dir out/...`) actually wires this.
        let tmp = tempfile::tempdir().expect("tempdir");
        // Chdir into tmp so a *relative* out_dir doesn't pollute the repo.
        // We can't use std::env::set_current_dir in multi-threaded tests
        // safely, so instead we construct a relative path that we'll turn
        // into an absolute tempdir-rooted path only when asserting — the
        // engine itself must handle the absolute-ification internally.
        // To do that, give the engine a path rooted in `tmp` but constructed
        // without canonicalization, simulating a relative-ish input.
        let out_dir = tmp.path().join("out").join("legacy-monolithic-test");

        // Write a one-phase workflow with no produces_files and no
        // assignments.json — this is the pure legacy monolithic path.
        let workflows_dir = tmp.path().join("workflows");
        tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
        let wf_path = workflows_dir.join("legacy-monolithic.json");
        let wf_json = r#"{
            "name": "legacy-monolithic",
            "description": "single-phase monolithic code path",
            "phases": [
                {
                    "name": "code",
                    "agent": "code-mock",
                    "context_template": "{{task}}"
                }
            ]
        }"#;
        tokio::fs::write(&wf_path, wf_json).await.unwrap();

        let captured: Arc<Mutex<Vec<Option<PathBuf>>>> = Arc::new(Mutex::new(Vec::new()));
        let mock = Arc::new(WorkingDirCapture {
            working_dirs: captured.clone(),
        });

        let engine = WorkflowEngine::new(mock, workflows_dir.clone());
        engine
            .run(
                "legacy-monolithic",
                "do the thing".into(),
                Some(out_dir.clone()),
            )
            .await
            .expect("workflow should complete");

        // The mock must have been called through `run_with_context` (which
        // is what the legacy monolithic branch uses — not plain `run`).
        let snap = captured.lock().unwrap().clone();
        assert_eq!(
            snap.len(),
            1,
            "expected exactly one agent invocation, saw {snap:?}"
        );
        let observed = snap[0]
            .clone()
            .expect("legacy monolithic path must set RunContext::working_dir, got None");

        // Core assertion: working_dir must be absolute, not a relative path
        // that subprocesses could re-resolve against an inherited CWD.
        assert!(
            observed.is_absolute(),
            "legacy monolithic path must set an absolute working_dir; got {}",
            observed.display()
        );

        // And it must point at the canonical out_dir (same file after
        // canonicalization). We canonicalize our expected path the same
        // way the engine does, so symlink resolution (e.g. /var -> /private/var
        // on macOS) doesn't cause a spurious mismatch.
        let expected = std::fs::canonicalize(&out_dir).expect("canonicalize test out_dir");
        assert_eq!(
            observed, expected,
            "working_dir must match canonicalized out_dir"
        );
    }

    /// #88: Wave-loop mock that records each per-file task it receives so we
    /// can assert the wave loop invokes the code-agent once per assignment
    /// in the right order.
    struct WaveLoopMock {
        calls: Arc<Mutex<Vec<(String, String)>>>, // (agent, task)
        /// Snapshots of the `max_turns_override` observed at each call. None
        /// means the context carried no override (non-wave path).
        max_turns_snapshots: Arc<Mutex<Vec<Option<u32>>>>,
        out_dir: PathBuf,
    }

    #[async_trait]
    impl AgentRunner for WaveLoopMock {
        async fn run(&self, agent_name: &str, task: &str) -> Result<AgentOutput> {
            // CRIT-1 / MAJ-1 (#90, #93): Default `run` path is taken when
            // the engine calls us without a RunContext (legacy non-wave).
            // Record the call and log that no override was observed.
            self.calls
                .lock()
                .unwrap()
                .push((agent_name.to_string(), task.to_string()));
            self.max_turns_snapshots.lock().unwrap().push(None);

            Ok(AgentOutput {
                content: format!("done {agent_name}"),
                summary: Some("ok".into()),
                usage: TokenUsage::default(),
            })
        }

        async fn run_with_context(
            &self,
            agent_name: &str,
            task: &str,
            ctx: &RunContext,
        ) -> Result<AgentOutput> {
            self.calls
                .lock()
                .unwrap()
                .push((agent_name.to_string(), task.to_string()));

            // CRIT-1 / MAJ-1 (#90, #93): Snapshot the context-provided turn
            // cap so tests can prove the wave loop plumbed it through the
            // `RunContext` instead of mutating parent env vars.
            self.max_turns_snapshots
                .lock()
                .unwrap()
                .push(ctx.max_turns_override);

            // Simulate the agent writing its assigned file to disk using the
            // context-supplied path (previously came from an env var).
            if let Some(path) = &ctx.assigned_file {
                let dest = self.out_dir.join(path);
                if let Some(parent) = dest.parent() {
                    tokio::fs::create_dir_all(parent).await.ok();
                }
                tokio::fs::write(&dest, b"# generated\n").await.ok();
            }

            Ok(AgentOutput {
                content: format!("done {agent_name}"),
                summary: Some("ok".into()),
                usage: TokenUsage::default(),
            })
        }
    }

    // CRIT-1 (#90): The `env_lock()` helper previously serialized tests that
    // mutated `OPEN_MPM_ASSIGNED_FILE` / `OPEN_MPM_MAX_TURNS` globally. With
    // the wave loop now threading those overrides through a `RunContext` on
    // each call, no test touches process-global env vars, so no lock is needed.

    /// #88: With `assignments.json` present, the code phase invokes the runner
    /// once per file in wave order, sets `OPEN_MPM_ASSIGNED_FILE` for each
    /// call, and each file's prompt names the correct path.
    #[tokio::test]
    async fn wave_loop_runs_one_agent_per_file() {
        let tmp = tempfile::tempdir().unwrap();
        let out_dir = tmp.path().to_path_buf();

        // Seed assignments.json — two waves, three files total.
        let assignments_json = r#"{
            "error_convention": "exceptions",
            "waves": [
                {
                    "wave": 1,
                    "files": [
                        {"path":"src/util.py","stub":"util.py","purpose":"helpers"},
                        {"path":"src/types.py","stub":"types.py","purpose":"type defs"}
                    ]
                },
                {
                    "wave": 2,
                    "files": [
                        {"path":"src/main.py","stub":"main.py","purpose":"entrypoint",
                         "depends_on":["src/util.py","src/types.py"]}
                    ]
                }
            ]
        }"#;
        tokio::fs::write(out_dir.join("assignments.json"), assignments_json)
            .await
            .unwrap();

        // Minimal workflow with just the code phase.
        let workflows_dir = tmp.path().join("workflows");
        tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
        let wf_path = workflows_dir.join("wave-test.json");
        let wf_json = r#"{
            "name": "wave-test",
            "description": "wave loop",
            "phases": [
                {"name":"code","agent":"code-agent","context_template":"{{task}}"}
            ]
        }"#;
        tokio::fs::write(&wf_path, wf_json).await.unwrap();

        let calls: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let max_turns_snapshots: Arc<Mutex<Vec<Option<u32>>>> = Arc::new(Mutex::new(Vec::new()));
        let mock = Arc::new(WaveLoopMock {
            calls: calls.clone(),
            max_turns_snapshots: max_turns_snapshots.clone(),
            out_dir: out_dir.clone(),
        });
        let engine = WorkflowEngine::new(mock, workflows_dir.clone());
        engine
            .run("wave-test", "x".into(), Some(out_dir.clone()))
            .await
            .expect("wave-loop workflow ok");

        let recorded = calls.lock().unwrap().clone();
        // Three per-file invocations, all against code-agent.
        assert_eq!(recorded.len(), 3, "expected 3 calls, got {recorded:?}");
        assert!(recorded.iter().all(|(a, _)| a == "code-agent"));

        // Wave order: util.py, types.py, then main.py.
        assert!(recorded[0].1.contains("src/util.py"));
        assert!(recorded[1].1.contains("src/types.py"));
        assert!(recorded[2].1.contains("src/main.py"));

        // Main.py's prompt must list its dependencies.
        assert!(recorded[2].1.contains("src/util.py"));
        assert!(recorded[2].1.contains("src/types.py"));

        // Each per-file task mentions the stub read step.
        assert!(recorded[0].1.contains("stubs/util.py"));
        assert!(recorded[2].1.contains("stubs/main.py"));

        // Files landed on disk (written by the mock from ctx.assigned_file).
        assert!(out_dir.join("src/util.py").exists());
        assert!(out_dir.join("src/types.py").exists());
        assert!(out_dir.join("src/main.py").exists());

        // CRIT-1 (#90): Every per-file invocation must observe
        // max_turns_override=40 via the RunContext (not a process env var)
        // so the sub-agent's turn budget is adequate for complex files.
        let turns = max_turns_snapshots.lock().unwrap().clone();
        assert_eq!(turns.len(), 3);
        for (i, v) in turns.iter().enumerate() {
            assert_eq!(
                *v,
                Some(40),
                "call {i} expected max_turns_override=40, got {v:?}"
            );
        }

        // CRIT-1 (#90): Parent env must never be touched by the wave loop.
        assert!(std::env::var("OPEN_MPM_ASSIGNED_FILE").is_err());
        assert!(std::env::var("OPEN_MPM_MAX_TURNS").is_err());
    }

    /// #166: Regression test — the per-file wave-loop task must instruct the
    /// agent to call write_file with an ABSOLUTE path (out_dir + file.path).
    ///
    /// Why: The claude CLI subprocess anchors relative `write_file` calls to
    /// the git repository root, so a relative path like
    /// `multi_repo_analyzer/pyproject.toml` would land at the repo root
    /// instead of under out_dir.
    /// What: Run the wave loop with a known out_dir and assert each recorded
    /// task prompt contains `out_dir/<file.path>` and instructs the agent to
    /// use that absolute path for write_file.
    /// Test: This function — set up a temp out_dir, seed assignments.json
    /// with a relative file path, run the engine, and assert the mock's
    /// recorded prompt contains the absolute path and the ABSOLUTE PATH
    /// warning language.
    #[tokio::test]
    async fn wave_loop_task_uses_absolute_path_for_write() {
        let tmp = tempfile::tempdir().unwrap();
        let out_dir = tmp.path().to_path_buf();

        // Single file, single wave — minimal seed.
        let assignments_json = r#"{
            "error_convention": "exceptions",
            "waves": [
                {
                    "wave": 1,
                    "files": [
                        {"path":"multi_repo_analyzer/pyproject.toml","stub":"pyproject.toml","purpose":"build config"}
                    ]
                }
            ]
        }"#;
        tokio::fs::write(out_dir.join("assignments.json"), assignments_json)
            .await
            .unwrap();

        let workflows_dir = tmp.path().join("workflows");
        tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
        let wf_path = workflows_dir.join("wave-abs-test.json");
        let wf_json = r#"{
            "name": "wave-abs-test",
            "description": "wave loop absolute path",
            "phases": [
                {"name":"code","agent":"code-agent","context_template":"{{task}}"}
            ]
        }"#;
        tokio::fs::write(&wf_path, wf_json).await.unwrap();

        let calls: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let max_turns_snapshots: Arc<Mutex<Vec<Option<u32>>>> = Arc::new(Mutex::new(Vec::new()));
        let mock = Arc::new(WaveLoopMock {
            calls: calls.clone(),
            max_turns_snapshots: max_turns_snapshots.clone(),
            out_dir: out_dir.clone(),
        });
        let engine = WorkflowEngine::new(mock, workflows_dir.clone());
        engine
            .run("wave-abs-test", "x".into(), Some(out_dir.clone()))
            .await
            .expect("wave-loop workflow ok");

        let recorded = calls.lock().unwrap().clone();
        assert_eq!(recorded.len(), 1, "expected 1 call, got {recorded:?}");
        let prompt = &recorded[0].1;

        // The prompt must contain the absolute path (out_dir + file.path).
        let expected_abs = out_dir.join("multi_repo_analyzer/pyproject.toml");
        let expected_abs_str = expected_abs.to_string_lossy().to_string();
        assert!(
            prompt.contains(&expected_abs_str),
            "task prompt missing absolute path `{expected_abs_str}`; prompt was:\n{prompt}"
        );

        // The prompt must use ABSOLUTE PATH language in the write step so
        // agents cannot miss the instruction.
        assert!(
            prompt.contains("ABSOLUTE PATH"),
            "task prompt missing ABSOLUTE PATH emphasis; prompt was:\n{prompt}"
        );
        assert!(
            prompt.contains("write_file"),
            "task prompt missing write_file reference; prompt was:\n{prompt}"
        );

        // The relative path is still present (for reading stubs/deps and
        // human-readable context), but the write step explicitly warns
        // against using it for writing.
        assert!(prompt.contains("multi_repo_analyzer/pyproject.toml"));
        assert!(
            prompt.contains("will land in the wrong directory"),
            "task prompt missing 'wrong directory' warning; prompt was:\n{prompt}"
        );
    }

    /// Mock runner for the "plan writes assignments.json then code uses
    /// wave-loop" regression test. The plan agent writes assignments.json
    /// during its run (simulating what the real plan-agent's write_file tool
    /// does). The code agent records each call it receives.
    struct PlanThenCodeMock {
        /// (agent, task) pairs recorded in order.
        calls: Arc<Mutex<Vec<(String, String)>>>,
        /// Directory where the "plan" step will write assignments.json and
        /// where the "code" step expects to find it.
        out_dir: PathBuf,
        /// Body to write as assignments.json when the plan agent runs.
        assignments_body: String,
    }

    #[async_trait]
    impl AgentRunner for PlanThenCodeMock {
        async fn run(&self, agent_name: &str, task: &str) -> Result<AgentOutput> {
            // Non-wave calls (plan phase) land here; wave-loop calls go
            // through `run_with_context` below.
            self.calls
                .lock()
                .unwrap()
                .push((agent_name.to_string(), task.to_string()));

            if agent_name == "plan-mock" {
                // Simulate the plan-agent's write_file("assignments.json", ...)
                // that happens DURING plan phase execution. The engine's
                // wave-loop decision must observe this write when it later
                // processes the code phase.
                tokio::fs::write(
                    self.out_dir.join("assignments.json"),
                    &self.assignments_body,
                )
                .await
                .ok();
                return Ok(AgentOutput {
                    content: "plan done".into(),
                    summary: Some("planned".into()),
                    usage: TokenUsage::default(),
                });
            }

            Ok(AgentOutput {
                content: format!("done {agent_name}"),
                summary: Some("ok".into()),
                usage: TokenUsage::default(),
            })
        }

        async fn run_with_context(
            &self,
            agent_name: &str,
            task: &str,
            ctx: &RunContext,
        ) -> Result<AgentOutput> {
            self.calls
                .lock()
                .unwrap()
                .push((agent_name.to_string(), task.to_string()));

            // plan-mock goes through non-wave path which may or may not set
            // working_dir; still simulate its assignments.json write.
            if agent_name == "plan-mock" {
                tokio::fs::write(
                    self.out_dir.join("assignments.json"),
                    &self.assignments_body,
                )
                .await
                .ok();
                return Ok(AgentOutput {
                    content: "plan done".into(),
                    summary: Some("planned".into()),
                    usage: TokenUsage::default(),
                });
            }

            // code-agent: wave-loop writes each per-file output using the
            // context-supplied assigned_file.
            if let Some(path) = &ctx.assigned_file {
                let dest = self.out_dir.join(path);
                if let Some(parent) = dest.parent() {
                    tokio::fs::create_dir_all(parent).await.ok();
                }
                tokio::fs::write(&dest, b"# generated\n").await.ok();
            }

            Ok(AgentOutput {
                content: format!("done {agent_name}"),
                summary: Some("ok".into()),
                usage: TokenUsage::default(),
            })
        }
    }

    /// #88 regression (post-merge bug): The wave-loop trigger must check
    /// `out_dir/assignments.json` AFTER the plan phase has written it, not
    /// before. This test exercises the full two-phase (plan -> code) path
    /// with no pre-seeded assignments.json — the plan mock writes it at
    /// runtime. If the engine checked for assignments.json at startup (or
    /// only before the first phase), the code phase would see nothing and
    /// fall through to the legacy path. We assert the wave loop fires by
    /// counting per-file code-agent invocations.
    #[tokio::test]
    async fn wave_loop_triggers_after_plan_phase_writes_assignments() {
        let tmp = tempfile::tempdir().unwrap();
        let out_dir = tmp.path().to_path_buf();

        // Deliberately NOT seeded — plan-mock writes this during its run.
        assert!(!out_dir.join("assignments.json").exists());

        let workflows_dir = tmp.path().join("workflows");
        tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
        let wf_path = workflows_dir.join("plan-then-code.json");
        let wf_json = r#"{
            "name": "plan-then-code",
            "phases": [
                {"name":"plan","agent":"plan-mock","context_template":"{{task}}"},
                {"name":"code","agent":"code-agent","context_template":"{{plan}}"}
            ]
        }"#;
        tokio::fs::write(&wf_path, wf_json).await.unwrap();

        let assignments_body = r#"{
            "error_convention": "exceptions",
            "waves": [
                {
                    "wave": 1,
                    "files": [
                        {"path":"src/a.py","stub":"a.py","purpose":"first"},
                        {"path":"src/b.py","stub":"b.py","purpose":"second"}
                    ]
                }
            ]
        }"#
        .to_string();

        let calls: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let mock = Arc::new(PlanThenCodeMock {
            calls: calls.clone(),
            out_dir: out_dir.clone(),
            assignments_body,
        });
        let engine = WorkflowEngine::new(mock, workflows_dir.clone());
        engine
            .run("plan-then-code", "x".into(), Some(out_dir.clone()))
            .await
            .expect("plan-then-code workflow ok");

        let recorded = calls.lock().unwrap().clone();

        // Expect exactly 3 calls: 1 plan + 2 per-file code-agent invocations.
        // If the wave loop didn't trigger, this would be 2 (plan + 1 code).
        assert_eq!(
            recorded.len(),
            3,
            "expected plan + 2 per-file code calls, got {recorded:?}"
        );
        assert_eq!(recorded[0].0, "plan-mock");
        assert_eq!(recorded[1].0, "code-agent");
        assert_eq!(recorded[2].0, "code-agent");

        // Each per-file prompt must name its assigned file.
        assert!(
            recorded[1].1.contains("src/a.py"),
            "first code call missing src/a.py: {}",
            recorded[1].1
        );
        assert!(
            recorded[2].1.contains("src/b.py"),
            "second code call missing src/b.py: {}",
            recorded[2].1
        );

        // Assignments.json was written by plan-mock and survived the run.
        assert!(out_dir.join("assignments.json").exists());

        // Both assigned files landed on disk (written by the mock via env var).
        assert!(out_dir.join("src/a.py").exists());
        assert!(out_dir.join("src/b.py").exists());
    }

    /// #88: A workflow without assignments.json runs the code phase the old
    /// way — one invocation of the code-agent, not per-file.
    #[tokio::test]
    async fn wave_loop_skipped_when_assignments_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let out_dir = tmp.path().to_path_buf();
        // Intentionally: no assignments.json written.

        let workflows_dir = tmp.path().join("workflows");
        tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
        let wf_path = workflows_dir.join("bc-test.json");
        let wf_json = r#"{
            "name": "bc-test",
            "phases": [
                {"name":"code","agent":"code-agent","context_template":"{{task}}"}
            ]
        }"#;
        tokio::fs::write(&wf_path, wf_json).await.unwrap();

        let calls: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let max_turns_snapshots: Arc<Mutex<Vec<Option<u32>>>> = Arc::new(Mutex::new(Vec::new()));
        let mock = Arc::new(WaveLoopMock {
            calls: calls.clone(),
            max_turns_snapshots: max_turns_snapshots.clone(),
            out_dir: out_dir.clone(),
        });
        let engine = WorkflowEngine::new(mock, workflows_dir.clone());
        engine
            .run("bc-test", "x".into(), Some(out_dir.clone()))
            .await
            .expect("bc workflow ok");

        // Exactly one call — the single monolithic code-agent invocation.
        let recorded = calls.lock().unwrap().clone();
        assert_eq!(recorded.len(), 1, "expected 1 call, got {recorded:?}");

        // CRIT-1 (#90): Legacy (non-wave) path must NOT supply a
        // max_turns_override so the single-shot invocation honors the agent
        // TOML's default. The RunContext carries `None` for non-wave calls.
        let turns = max_turns_snapshots.lock().unwrap().clone();
        assert_eq!(turns, vec![None]);
    }

    /// #108/#109: an engine configured with an `InitContext` must prepend
    /// the project summary + memories prefix to every phase's rendered task.
    /// We use a recording mock that captures the exact task text the runner
    /// receives and assert the prefix appears before the task body.
    #[tokio::test]
    async fn init_context_is_prepended_to_phase_template() {
        struct RecordingRunner {
            tasks: Arc<Mutex<Vec<String>>>,
        }
        #[async_trait]
        impl AgentRunner for RecordingRunner {
            async fn run(&self, _agent_name: &str, task: &str) -> Result<AgentOutput> {
                self.tasks.lock().unwrap().push(task.to_string());
                Ok(AgentOutput {
                    content: "ok".into(),
                    summary: None,
                    usage: TokenUsage::default(),
                })
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let workflows_dir = tmp.path().join("workflows");
        tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
        let wf_path = workflows_dir.join("init-test.json");
        let wf_json = r#"{
            "name": "init-test",
            "description": "single phase",
            "phases": [
                {"name":"research","agent":"research-agent","context_template":"TASK={{task}}"}
            ]
        }"#;
        tokio::fs::write(&wf_path, wf_json).await.unwrap();

        let tasks: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let mock = Arc::new(RecordingRunner {
            tasks: tasks.clone(),
        });

        let ic = InitContext {
            project_summary: "# Project: demo\nindex body".into(),
            relevant_memories: vec!["prior fact".into()],
            initialized_at: chrono::Utc::now(),
        };

        let engine = WorkflowEngine::new(mock, workflows_dir.clone()).with_init_context(Some(ic));
        let _ = engine
            .run("init-test", "my-task".into(), None)
            .await
            .expect("workflow ok");

        let recorded = tasks.lock().unwrap().clone();
        assert_eq!(recorded.len(), 1);
        let seen = &recorded[0];
        assert!(
            seen.contains("## Project Context (auto-indexed)"),
            "seen: {seen}"
        );
        assert!(seen.contains("prior fact"), "seen: {seen}");
        assert!(seen.contains("TASK=my-task"), "seen: {seen}");
        // Ordering: prefix must come before task body.
        let pidx = seen.find("Project Context").unwrap();
        let tidx = seen.find("TASK=my-task").unwrap();
        assert!(pidx < tidx, "prefix should appear before task body");
    }

    // ---- #150: Pre-create empty package markers ----

    fn fa_raw(
        path: &str,
        stub: Option<&str>,
        purpose: &str,
    ) -> crate::workflow::config::FileAssignment {
        crate::workflow::config::FileAssignment {
            path: path.to_string(),
            stub: stub.map(String::from),
            purpose: purpose.to_string(),
            depends_on: Vec::new(),
            max_lines: None,
        }
    }

    #[test]
    fn should_precreate_detects_init_py() {
        // #150: __init__.py always qualifies for pre-creation, even when the
        // plan-agent attached a stub and purpose — engineers still skip them.
        let f = fa_raw("pkg/__init__.py", Some("init.py"), "package marker");
        assert!(should_precreate(&f));

        let f2 = fa_raw("a/b/c/__init__.py", None, "");
        assert!(should_precreate(&f2));
    }

    #[test]
    fn should_precreate_detects_empty_stub_and_purpose() {
        // #150: stub:null + empty purpose signals a plan-agent placeholder.
        let f = fa_raw("src/placeholder.py", None, "");
        assert!(should_precreate(&f));

        let f_ws = fa_raw("src/placeholder.py", None, "   ");
        assert!(should_precreate(&f_ws));
    }

    #[test]
    fn should_precreate_rejects_normal_file() {
        // #150: Normal files with real purpose are left to the engineer.
        let f = fa_raw("src/main.py", Some("main.py"), "entrypoint");
        assert!(!should_precreate(&f));

        let f_no_stub = fa_raw("src/main.py", None, "entrypoint logic");
        assert!(!should_precreate(&f_no_stub));
    }

    #[tokio::test]
    async fn precreate_package_markers_creates_init_py() {
        // #150: Given an assignments plan with an __init__.py, pre-creation
        // writes an empty file at the expected path under out_dir so the
        // wave-loop presence check passes even if the agent skips it.
        let tmp = tempfile::tempdir().unwrap();
        let out_dir = tmp.path();

        let assignments = Assignments {
            error_convention: None,
            waves: vec![crate::workflow::config::WaveDef {
                wave: 1,
                files: vec![
                    fa_raw("git_analyzer/src/git_analyzer/__init__.py", None, ""),
                    fa_raw("src/main.py", Some("main.py"), "entrypoint"),
                ],
            }],
        };

        precreate_package_markers(&assignments, out_dir)
            .await
            .expect("pre-create ok");

        let init = out_dir.join("git_analyzer/src/git_analyzer/__init__.py");
        assert!(init.exists(), "__init__.py should be pre-created");
        let content = tokio::fs::read(&init).await.unwrap();
        assert!(content.is_empty(), "pre-created file must be empty");

        // Non-placeholder file must NOT be pre-created.
        let main = out_dir.join("src/main.py");
        assert!(!main.exists(), "main.py should be left to the engineer");
    }

    #[tokio::test]
    async fn precreate_package_markers_preserves_existing_content() {
        // #150: If a file already exists (e.g. from a prior wave), pre-create
        // must NOT overwrite it.
        let tmp = tempfile::tempdir().unwrap();
        let out_dir = tmp.path();

        let existing = out_dir.join("pkg/__init__.py");
        tokio::fs::create_dir_all(existing.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&existing, b"from .x import *\n")
            .await
            .unwrap();

        let assignments = Assignments {
            error_convention: None,
            waves: vec![crate::workflow::config::WaveDef {
                wave: 1,
                files: vec![fa_raw("pkg/__init__.py", None, "")],
            }],
        };

        precreate_package_markers(&assignments, out_dir)
            .await
            .expect("pre-create ok");

        let content = tokio::fs::read(&existing).await.unwrap();
        assert_eq!(content, b"from .x import *\n");
    }

    /// #160: Regression test — if the plan-agent writes `assignments.json`
    /// at the git project root (because its claude CLI anchors relative
    /// Write-tool paths to the inherited CWD instead of
    /// `RunContext::working_dir`), the post-plan relocation step must move
    /// it into `out_dir` so the wave-loop check succeeds.
    #[tokio::test]
    async fn post_plan_relocates_assignments_json_from_git_root() {
        // Arrange: separate simulated project root and out_dir.
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path().join("project");
        let out_dir = tmp.path().join("out");
        tokio::fs::create_dir_all(&project_root).await.unwrap();
        tokio::fs::create_dir_all(&out_dir).await.unwrap();

        // Simulate plan-agent's misroute: assignments.json lands at project
        // root instead of out_dir.
        let misrouted = project_root.join("assignments.json");
        let body = r#"{"error_convention":"exceptions","waves":[{"wave":1,"files":[{"path":"app/main.py","stub":"main.py","purpose":"entry","depends_on":[],"max_lines":100}]}]}"#;
        tokio::fs::write(&misrouted, body).await.unwrap();

        // Also simulate a stubs/ directory at project root.
        let misrouted_stubs = project_root.join("stubs");
        tokio::fs::create_dir_all(&misrouted_stubs).await.unwrap();
        tokio::fs::write(misrouted_stubs.join("main.py"), b"# stub")
            .await
            .unwrap();

        // Act: run the relocation logic against the simulated project root.
        relocate_plan_outputs_from(&project_root, &out_dir)
            .await
            .expect("relocation should succeed");

        // Assert: assignments.json is now in out_dir with correct contents.
        let relocated = out_dir.join("assignments.json");
        assert!(
            relocated.is_file(),
            "assignments.json should be relocated to out_dir, but {} is missing",
            relocated.display()
        );
        let read_body = tokio::fs::read_to_string(&relocated).await.unwrap();
        assert_eq!(
            read_body, body,
            "relocated assignments.json content mismatch"
        );

        // Assert: the misrouted file at project root is gone (moved, not copied).
        assert!(
            !misrouted.exists(),
            "misrouted assignments.json should be removed from project root after relocation"
        );

        // Assert: stubs/ was also relocated.
        let relocated_stubs = out_dir.join("stubs");
        assert!(
            relocated_stubs.is_dir(),
            "stubs/ should be relocated to out_dir"
        );
        assert!(
            relocated_stubs.join("main.py").is_file(),
            "stubs/main.py should be present at out_dir/stubs/main.py"
        );
    }

    /// #160: If `out_dir/assignments.json` already exists, relocation is a
    /// no-op — we must not clobber the happy-path output with whatever is
    /// sitting at the project root.
    #[tokio::test]
    async fn post_plan_relocation_is_noop_when_out_dir_has_assignments() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path().join("project");
        let out_dir = tmp.path().join("out");
        tokio::fs::create_dir_all(&project_root).await.unwrap();
        tokio::fs::create_dir_all(&out_dir).await.unwrap();

        let good = out_dir.join("assignments.json");
        tokio::fs::write(&good, b"GOOD").await.unwrap();

        // Planted stale/bad file at project root that must NOT be moved.
        let stale = project_root.join("assignments.json");
        tokio::fs::write(&stale, b"STALE").await.unwrap();

        relocate_plan_outputs_from(&project_root, &out_dir)
            .await
            .expect("noop relocation ok");

        assert_eq!(tokio::fs::read(&good).await.unwrap(), b"GOOD");
        assert!(
            stale.exists(),
            "stale file at project root must not be removed"
        );
    }

    // ── #173: pre-plan skill discovery ────────────────────────────────────
    //
    // Why: The engine should derive `TaskSignals` from the task text, query
    // the tag-indexed registry, and prepend a "## Available Skills" block to
    // the plan-agent's prompt — without the plan-agent ever calling
    // `list_skills`. These tests pin the contract.

    /// Build a tag-indexed registry from a fresh temp dir containing a few
    /// `python` / `fastapi` / `pytest` skills.
    fn temp_tag_registry_with_python_skills() -> (tempfile::TempDir, Arc<TagSkillRegistry>) {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, desc: &str, tags: &[&str]| {
            let tags_str = tags
                .iter()
                .map(|t| format!("\"{t}\""))
                .collect::<Vec<_>>()
                .join(", ");
            let content = format!(
                "---\nname: {name}\ndescription: {desc}\ntags: [{tags_str}]\n---\n\n# {name}\nbody\n",
            );
            std::fs::write(dir.path().join(format!("{name}.md")), content).unwrap();
        };
        write(
            "fastapi",
            "FastAPI application patterns, TestClient usage, module-level state",
            &["python", "fastapi", "api"],
        );
        write(
            "pytest",
            "async fixtures, parametrize, conftest patterns",
            &["python", "testing", "pytest"],
        );
        write(
            "python",
            "type hints, dataclasses, NLP setup",
            &["python", "packaging"],
        );
        write("rust", "Rust patterns", &["rust", "tokio"]);

        let reg = TagSkillRegistry::load(&[dir.path().to_path_buf()]);
        (dir, Arc::new(reg))
    }

    /// #173: discovery must pull `python` + `fastapi` skills out of the
    /// tag-indexed registry given a task that mentions Python and FastAPI.
    /// The Rust-only skill must NOT appear because no rust signals match.
    #[test]
    fn skill_discovery_extracts_python_fastapi_tags() {
        let (_keep, reg) = temp_tag_registry_with_python_skills();

        struct NopRunner;
        #[async_trait]
        impl AgentRunner for NopRunner {
            async fn run(&self, _: &str, _: &str) -> Result<AgentOutput> {
                Ok(AgentOutput {
                    content: String::new(),
                    summary: None,
                    usage: TokenUsage::default(),
                })
            }
        }

        let engine = WorkflowEngine::new(Arc::new(NopRunner), PathBuf::from("."))
            .with_tag_skill_registry(Some(reg));

        let task = "Build a Python FastAPI service with pytest tests for the REST endpoints";
        let discovered = engine.discover_skills_for_task(task, 8);

        let names: Vec<&str> = discovered.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"fastapi"),
            "fastapi should be discovered: got {names:?}"
        );
        assert!(
            names.contains(&"pytest"),
            "pytest should be discovered: got {names:?}"
        );
        assert!(
            names.contains(&"python"),
            "python should be discovered: got {names:?}"
        );
        assert!(
            !names.contains(&"rust"),
            "rust must not be matched: got {names:?}"
        );

        // Each discovered skill carries a non-empty summary + tags.
        for s in &discovered {
            assert!(!s.summary.is_empty(), "summary empty for {}", s.name);
            assert!(!s.tags.is_empty(), "tags empty for {}", s.name);
        }
    }

    /// #173: when many skills tie on raw tag-overlap, effectiveness scores
    /// drive the top-N ordering — the engine must respect the registry's
    /// ranking and only return the top `limit`.
    #[test]
    fn skill_discovery_returns_top_n_by_effectiveness() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, desc: &str, tags: &[&str]| {
            let tags_str = tags
                .iter()
                .map(|t| format!("\"{t}\""))
                .collect::<Vec<_>>()
                .join(", ");
            let content = format!(
                "---\nname: {name}\ndescription: {desc}\ntags: [{tags_str}]\n---\n\nbody\n",
            );
            std::fs::write(dir.path().join(format!("{name}.md")), content).unwrap();
        };
        // Five skills, all matching the single "python" tag — effectiveness
        // breaks the tie. Discovery order (insertion) is the secondary
        // tie-breaker so we drive ranking purely via effectiveness.
        write("a", "d", &["python"]);
        write("b", "d", &["python"]);
        write("c", "d", &["python"]);
        write("d", "d", &["python"]);
        write("e", "d", &["python"]);

        let mut reg = TagSkillRegistry::load(&[dir.path().to_path_buf()]);
        // Push c and a to the top via effectiveness boost.
        reg.update_effectiveness("c", 1.0);
        reg.update_effectiveness("c", 1.0);
        reg.update_effectiveness("c", 1.0);
        reg.update_effectiveness("a", 1.0);

        struct NopRunner;
        #[async_trait]
        impl AgentRunner for NopRunner {
            async fn run(&self, _: &str, _: &str) -> Result<AgentOutput> {
                Ok(AgentOutput {
                    content: String::new(),
                    summary: None,
                    usage: TokenUsage::default(),
                })
            }
        }

        let engine = WorkflowEngine::new(Arc::new(NopRunner), PathBuf::from("."))
            .with_tag_skill_registry(Some(Arc::new(reg)));

        let discovered = engine.discover_skills_for_task("Write a python script", 2);
        assert_eq!(discovered.len(), 2, "limit must be honored");
        // The boosted skills should come first; we don't assert exact order
        // beyond "c is first" because effectiveness EMA + tie-breakers can
        // shift across same-effectiveness siblings.
        assert_eq!(
            discovered[0].name,
            "c",
            "highest-effectiveness skill should rank first; got {:?}",
            discovered.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
    }

    /// #173: discovery returns empty when the registry is absent or empty —
    /// the engine must NOT panic and must NOT inject anything into the
    /// plan-agent prompt downstream.
    #[test]
    fn skill_discovery_returns_empty_when_registry_absent() {
        struct NopRunner;
        #[async_trait]
        impl AgentRunner for NopRunner {
            async fn run(&self, _: &str, _: &str) -> Result<AgentOutput> {
                Ok(AgentOutput {
                    content: String::new(),
                    summary: None,
                    usage: TokenUsage::default(),
                })
            }
        }

        let engine = WorkflowEngine::new(Arc::new(NopRunner), PathBuf::from("."));
        let discovered = engine.discover_skills_for_task("python fastapi", 8);
        assert!(discovered.is_empty(), "no registry → empty discovery");

        let empty_reg = Arc::new(TagSkillRegistry::empty());
        let engine = engine.with_tag_skill_registry(Some(empty_reg));
        let discovered = engine.discover_skills_for_task("python fastapi", 8);
        assert!(discovered.is_empty(), "empty registry → empty discovery");
    }

    /// #173: end-to-end — when the engine runs a workflow whose `plan` phase
    /// matches discovered skills, the runner sees the assembled task text
    /// containing the "## Available Skills" header. Other phases must NOT
    /// receive that block.
    #[tokio::test]
    async fn plan_agent_context_includes_skill_summaries() {
        let (_keep, reg) = temp_tag_registry_with_python_skills();

        struct RecordingRunner {
            tasks: Arc<Mutex<Vec<(String, String)>>>,
        }
        #[async_trait]
        impl AgentRunner for RecordingRunner {
            async fn run(&self, agent: &str, task: &str) -> Result<AgentOutput> {
                self.tasks
                    .lock()
                    .unwrap()
                    .push((agent.to_string(), task.to_string()));
                Ok(AgentOutput {
                    content: "ok".into(),
                    summary: None,
                    usage: TokenUsage::default(),
                })
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let workflows_dir = tmp.path().join("workflows");
        tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
        let wf_path = workflows_dir.join("plan-skills.json");
        // Two phases: a research phase (must NOT get the block) and a plan
        // phase (must receive it).
        let wf_json = r#"{
            "name": "plan-skills",
            "phases": [
                {"name":"research","agent":"research-agent","context_template":"R={{task}}"},
                {"name":"plan","agent":"plan-agent","context_template":"P={{task}}"}
            ]
        }"#;
        tokio::fs::write(&wf_path, wf_json).await.unwrap();

        let tasks: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let mock = Arc::new(RecordingRunner {
            tasks: tasks.clone(),
        });

        let engine =
            WorkflowEngine::new(mock, workflows_dir.clone()).with_tag_skill_registry(Some(reg));
        // #196: pin persona to engineer so the persona heuristic doesn't
        // accidentally classify this as "hacker" (the substring `fast` in
        // "FastAPI" matches the hacker keyword) and skip the research phase.
        engine
            .run(
                "plan-skills",
                "[engineer] Build a Python FastAPI service with pytest tests".into(),
                None,
            )
            .await
            .expect("workflow ok");

        let recorded = tasks.lock().unwrap().clone();
        assert_eq!(recorded.len(), 2);

        let (research_agent, research_task) = &recorded[0];
        assert_eq!(research_agent, "research-agent");
        assert!(
            !research_task.contains("## Available Skills"),
            "research phase must not receive the discovery block: {research_task}"
        );

        let (plan_agent, plan_task) = &recorded[1];
        assert_eq!(plan_agent, "plan-agent");
        assert!(
            plan_task.contains("## Available Skills"),
            "plan phase prompt must contain '## Available Skills': {plan_task}"
        );
        // The block must precede the rendered template body.
        let header_idx = plan_task.find("## Available Skills").unwrap();
        let body_idx = plan_task.find("P=Build a Python").expect("body present");
        assert!(
            header_idx < body_idx,
            "skills block must come before the task body"
        );
    }

    // ── #123: post-code reconciliation + QA path injection ─────────────────

    /// #123: When the code phase ran under a `claude-code` runner and that
    /// runner wrote a file declared in `assignments.json` to the project
    /// root instead of `out_dir`, the post-code reconciliation step must
    /// move it into `out_dir` so QA finds it.
    #[tokio::test]
    async fn post_code_reconciles_files_from_project_root() {
        // Arrange: separate simulated project_root and out_dir, plus an
        // assignments.json declaring two files. One file lands at out_dir
        // (happy path). The other lands at project_root (misroute) and must
        // be relocated.
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path().join("project");
        let out_dir = tmp.path().join("out");
        tokio::fs::create_dir_all(&project_root).await.unwrap();
        tokio::fs::create_dir_all(&out_dir).await.unwrap();

        // Plant assignments.json into out_dir so reconcile can read it.
        let asg_body = r#"{"error_convention":"exceptions","waves":[{"wave":1,"files":[{"path":"src/good.py","stub":null,"purpose":"on-disk","depends_on":[],"max_lines":100},{"path":"src/stray.py","stub":null,"purpose":"misrouted","depends_on":[],"max_lines":100}]}]}"#;
        tokio::fs::write(out_dir.join("assignments.json"), asg_body)
            .await
            .unwrap();

        // good.py is already in out_dir (happy path).
        tokio::fs::create_dir_all(out_dir.join("src"))
            .await
            .unwrap();
        tokio::fs::write(out_dir.join("src/good.py"), b"# good")
            .await
            .unwrap();

        // stray.py landed at project_root instead — this is the misroute we
        // need to reconcile.
        tokio::fs::create_dir_all(project_root.join("src"))
            .await
            .unwrap();
        tokio::fs::write(project_root.join("src/stray.py"), b"# stray")
            .await
            .unwrap();

        // Act: run reconciliation with the simulated project_root.
        reconcile_code_outputs_from(&project_root, &out_dir)
            .await
            .expect("reconciliation should succeed");

        // Assert: stray.py is now in out_dir, with correct content.
        let relocated = out_dir.join("src/stray.py");
        assert!(
            relocated.is_file(),
            "stray.py should be relocated into out_dir, but {} is missing",
            relocated.display()
        );
        let body = tokio::fs::read(&relocated).await.unwrap();
        assert_eq!(body, b"# stray", "relocated content mismatch");

        // Assert: project_root no longer holds the stray file.
        assert!(
            !project_root.join("src/stray.py").exists(),
            "stray.py should be removed from project_root after relocation"
        );

        // Assert: good.py was untouched (still at out_dir).
        let good_body = tokio::fs::read(out_dir.join("src/good.py")).await.unwrap();
        assert_eq!(good_body, b"# good");
    }

    /// #123: Reconciliation refuses to act on an unsafe path even if a
    /// malicious assignments.json slipped past validation. We simulate this
    /// by writing assignments.json directly (bypassing `Assignments::load`'s
    /// validator is not actually possible, so we instead verify the reconcile
    /// step skips when no assignments are present — the safe default).
    #[tokio::test]
    async fn post_code_reconcile_is_noop_without_assignments() {
        let tmp = tempfile::tempdir().unwrap();
        let project_root = tmp.path().join("project");
        let out_dir = tmp.path().join("out");
        tokio::fs::create_dir_all(&project_root).await.unwrap();
        tokio::fs::create_dir_all(&out_dir).await.unwrap();

        // No assignments.json in out_dir. Plant a file at project_root that
        // would otherwise be a tempting target.
        tokio::fs::write(project_root.join("src.py"), b"# no plan")
            .await
            .unwrap();

        reconcile_code_outputs_from(&project_root, &out_dir)
            .await
            .expect("noop reconcile ok");

        // The file must still be at project_root — without assignments.json
        // we have no list of files to reconcile, so we touch nothing.
        assert!(project_root.join("src.py").exists());
        assert!(!out_dir.join("src.py").exists());
    }

    /// #123: When the code phase ran under a `claude-code` runner, the QA
    /// phase's rendered task must include the project root path so QA knows
    /// where to look for any files that escaped reconciliation. Verifies the
    /// engine prepends the path-search hint to the QA prompt, while leaving
    /// it absent for non-claude-code runners.
    #[tokio::test]
    async fn qa_receives_correct_path_for_claude_code_runner() {
        // Arrange: a workflow with a code phase that uses an agent backed by
        // the claude-code runner, then a QA phase. We capture the rendered
        // prompts the runner sees.
        struct CapturingRunner {
            tasks: Arc<Mutex<Vec<(String, String)>>>,
        }
        #[async_trait]
        impl AgentRunner for CapturingRunner {
            async fn run(&self, agent: &str, task: &str) -> Result<AgentOutput> {
                self.tasks
                    .lock()
                    .unwrap()
                    .push((agent.to_string(), task.to_string()));
                Ok(AgentOutput {
                    content: "ok".into(),
                    summary: None,
                    usage: TokenUsage::default(),
                })
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let workflows_dir = tmp.path().join("workflows");
        tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
        let wf_path = workflows_dir.join("qa-path.json");
        // The "engineer" agent ships with `runner = "claude-code"` in its
        // bundled TOML, so the engine sets `code_phase_used_claude_code`.
        let wf_json = r#"{
            "name": "qa-path",
            "phases": [
                {"name":"code","agent":"engineer","context_template":"CODE={{task}}"},
                {"name":"qa","agent":"qa-agent","context_template":"QA={{task}} ROOT={{project_root}}"}
            ]
        }"#;
        tokio::fs::write(&wf_path, wf_json).await.unwrap();

        let tasks: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let mock = Arc::new(CapturingRunner {
            tasks: tasks.clone(),
        });

        let out_dir = tmp.path().join("out");
        let engine = WorkflowEngine::new(mock, workflows_dir.clone());
        engine
            .run("qa-path", "build it".into(), Some(out_dir.clone()))
            .await
            .expect("workflow ok");

        let recorded = tasks.lock().unwrap().clone();
        assert_eq!(
            recorded.len(),
            2,
            "expected code + qa calls, got {recorded:?}"
        );
        let (qa_agent, qa_task) = &recorded[1];
        assert_eq!(qa_agent, "qa-agent");

        // The QA prompt must contain the path-search hint AND the resolved
        // project_root from {{project_root}} substitution.
        assert!(
            qa_task.contains("claude-code runner was used"),
            "QA prompt missing claude-code hint: {qa_task}"
        );
        let cwd = std::env::current_dir().unwrap().display().to_string();
        assert!(
            qa_task.contains(&cwd),
            "QA prompt should include project_root path '{cwd}': {qa_task}"
        );
        // The hint must appear BEFORE the rendered template body.
        let hint_idx = qa_task.find("claude-code runner was used").unwrap();
        let body_idx = qa_task.find("QA=build it").expect("body present");
        assert!(
            hint_idx < body_idx,
            "claude-code hint must precede the rendered task body"
        );
    }

    // ── #206: Retry logic unit tests ─────────────────────────────────────────

    /// `is_retryable` must return true for textual 5xx / 429 / timeout signals.
    ///
    /// Why: The retry gate depends on string matching against the error message;
    /// these cases cover the most common Anthropic / OpenRouter transient errors.
    /// Test: assert true for each retryable string, false for a 400 / auth error.
    #[test]
    fn is_retryable_classifies_5xx() {
        let cases = [
            "API returned status 500: internal server error",
            "HTTP 502 bad gateway",
            "error 503 service unavailable",
            "status 529 overloaded",
            "HTTP 429 too many requests",
            "rate limit exceeded",
            "connection timed out after 30s",
            "connection reset by peer",
            "internal server error from upstream",
            "service unavailable, please retry",
            "overloaded — try again shortly",
            "broken pipe",
        ];
        for msg in &cases {
            let err = anyhow::anyhow!("{}", msg);
            assert!(is_retryable(&err), "expected retryable=true for: {msg}");
        }
    }

    /// `is_retryable` must return false for 4xx (non-429) and logic errors.
    ///
    /// Why: These errors will not resolve on retry; retrying them wastes time
    /// and could mask misconfigured agents or bad task prompts.
    /// Test: assert false for 400, 401, 403, 404, empty output, missing TOML.
    #[test]
    fn is_retryable_rejects_4xx() {
        let cases = [
            "status 400 bad request",
            "HTTP 401 unauthorized",
            "error 403 forbidden",
            "404 not found",
            "agent produced empty output",
            "failed to read agent config: no such file",
        ];
        for msg in &cases {
            let err = anyhow::anyhow!("{}", msg);
            assert!(!is_retryable(&err), "expected retryable=false for: {msg}");
        }
    }

    /// A transient 5xx error on the first call must be retried; success on the
    /// second call must be returned to the wave loop.
    ///
    /// Why: The original code returned the first error immediately; this test
    /// pins the new retry contract.
    /// What: Mock runner fails once with "status 500 internal server error"
    /// then succeeds; assert the final result is Ok and that two calls were
    /// made.
    /// Test: this function.
    #[tokio::test]
    async fn wave_loop_retries_on_transient_error() {
        use std::sync::atomic::{AtomicU32, Ordering};

        struct TransientMock {
            calls: AtomicU32,
            out_dir: PathBuf,
        }

        #[async_trait]
        impl AgentRunner for TransientMock {
            async fn run(&self, _: &str, _: &str) -> Result<AgentOutput> {
                unimplemented!("wave loop uses run_with_context")
            }

            async fn run_with_context(
                &self,
                _agent_name: &str,
                _task: &str,
                ctx: &RunContext,
            ) -> Result<AgentOutput> {
                let n = self.calls.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    // First call: simulate a transient 5xx.
                    return Err(anyhow::anyhow!(
                        "API returned status 500: internal server error"
                    ));
                }
                // Second call: succeed and write the file.
                if let Some(path) = &ctx.assigned_file {
                    let dest = self.out_dir.join(path);
                    if let Some(parent) = dest.parent() {
                        tokio::fs::create_dir_all(parent).await.ok();
                    }
                    tokio::fs::write(&dest, b"# ok\n").await.ok();
                }
                Ok(AgentOutput {
                    content: "success after retry".into(),
                    summary: Some("ok".into()),
                    usage: TokenUsage::default(),
                })
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let out_dir = tmp.path().to_path_buf();

        let assignments_json = r#"{
            "error_convention": "exceptions",
            "waves": [
                {
                    "wave": 1,
                    "files": [
                        {"path":"src/module.py","stub":null,"purpose":"main module"}
                    ]
                }
            ]
        }"#;
        tokio::fs::write(out_dir.join("assignments.json"), assignments_json)
            .await
            .unwrap();

        let workflows_dir = tmp.path().join("workflows");
        tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
        let wf_json = r#"{
            "name":"retry-test",
            "description":"retry on 5xx",
            "phases":[{"name":"code","agent":"eng","context_template":"{{task}}"}]
        }"#;
        tokio::fs::write(workflows_dir.join("retry-test.json"), wf_json)
            .await
            .unwrap();

        let mock = Arc::new(TransientMock {
            calls: AtomicU32::new(0),
            out_dir: out_dir.clone(),
        });
        let call_count = &mock.calls as *const AtomicU32;
        let engine = WorkflowEngine::new(mock, workflows_dir.clone());
        let result = engine
            .run("retry-test", "do it".into(), Some(out_dir.clone()))
            .await;

        assert!(result.is_ok(), "expected Ok after retry, got {result:?}");
        // SAFETY: mock outlives this assertion (it's in the Arc in the engine).
        let total_calls = unsafe { (*call_count).load(Ordering::SeqCst) };
        assert_eq!(total_calls, 2, "expected 2 calls (1 fail + 1 success)");
    }

    /// A fatal (non-retryable) error must NOT be retried; the wave loop must
    /// fail immediately after the first call.
    ///
    /// Why: Retrying 4xx errors wastes time and could mask misconfiguration.
    /// What: Mock runner always returns "status 400 bad request"; assert
    /// exactly one call is made and the error propagates.
    /// Test: this function.
    #[tokio::test]
    async fn wave_loop_does_not_retry_fatal_error() {
        use std::sync::atomic::{AtomicU32, Ordering};

        struct FatalMock {
            calls: AtomicU32,
        }

        #[async_trait]
        impl AgentRunner for FatalMock {
            async fn run(&self, _: &str, _: &str) -> Result<AgentOutput> {
                unimplemented!()
            }

            async fn run_with_context(
                &self,
                _: &str,
                _: &str,
                _: &RunContext,
            ) -> Result<AgentOutput> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Err(anyhow::anyhow!("status 400 bad request: invalid prompt"))
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let out_dir = tmp.path().to_path_buf();

        let assignments_json = r#"{
            "error_convention": "exceptions",
            "waves": [
                {
                    "wave": 1,
                    "files": [
                        {"path":"src/fail.py","stub":null,"purpose":"will fail"}
                    ]
                }
            ]
        }"#;
        tokio::fs::write(out_dir.join("assignments.json"), assignments_json)
            .await
            .unwrap();

        let workflows_dir = tmp.path().join("workflows");
        tokio::fs::create_dir_all(&workflows_dir).await.unwrap();
        let wf_json = r#"{
            "name":"fatal-test",
            "description":"no retry on 4xx",
            "phases":[{"name":"code","agent":"eng","context_template":"{{task}}"}]
        }"#;
        tokio::fs::write(workflows_dir.join("fatal-test.json"), wf_json)
            .await
            .unwrap();

        let mock = Arc::new(FatalMock {
            calls: AtomicU32::new(0),
        });
        let call_count = &mock.calls as *const AtomicU32;
        let engine = WorkflowEngine::new(mock, workflows_dir.clone());
        let result = engine
            .run("fatal-test", "do it".into(), Some(out_dir.clone()))
            .await;

        assert!(result.is_err(), "expected Err for fatal error");
        let total_calls = unsafe { (*call_count).load(Ordering::SeqCst) };
        assert_eq!(total_calls, 1, "fatal error must not be retried");
    }

    /// #231: After all transient retries on the base model fail, the wave
    /// loop must make ONE more attempt using the elevation model.
    ///
    /// Why: Engineer agents start on Sonnet for cost; some hard files require
    /// Opus. Elevation lets the harness automatically retry on a stronger
    /// model after repeated failures rather than requiring operator
    /// intervention.
    /// What: Mock runner always returns transient 5xx. With
    /// `elevation_threshold=2` and `elevation_model="claude-opus-4-6"`, after
    /// MAX_WAVE_RETRIES+1 base-model attempts fail, the runner must be called
    /// once more with `ctx.model = Some("claude-opus-4-6")`.
    /// Test: this function.
    #[tokio::test]
    async fn elevation_triggers_after_n_failures() {
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicU32, Ordering};

        struct ElevatingMock {
            calls: AtomicU32,
            seen_models: Mutex<Vec<Option<String>>>,
        }

        #[async_trait]
        impl AgentRunner for ElevatingMock {
            async fn run(&self, _: &str, _: &str) -> Result<AgentOutput> {
                unimplemented!("elevation test uses run_with_context")
            }

            async fn run_with_context(
                &self,
                _agent_name: &str,
                _task: &str,
                ctx: &RunContext,
            ) -> Result<AgentOutput> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.seen_models.lock().unwrap().push(ctx.model.clone());
                // If the runner sees the elevated model, succeed; otherwise
                // simulate transient 5xx errors so the retry loop exhausts.
                if ctx.model.as_deref() == Some("claude-opus-4-6") {
                    Ok(AgentOutput {
                        content: "elevated success".into(),
                        summary: Some("ok".into()),
                        usage: TokenUsage::default(),
                    })
                } else {
                    Err(anyhow::anyhow!(
                        "API returned status 503: service unavailable"
                    ))
                }
            }
        }

        let mock = ElevatingMock {
            calls: AtomicU32::new(0),
            seen_models: Mutex::new(Vec::new()),
        };

        let ctx = RunContext::default();
        // No sleep needed — but the retry helper does sleep with exponential
        // backoff. Use tokio's pause/auto-advance to keep the test fast.
        tokio::time::pause();
        let handle = tokio::spawn(async move {
            let result = run_wave_file_with_retry(
                &mock,
                "engineer",
                "build it",
                &ctx,
                Some(2),
                Some("claude-opus-4-6"),
            )
            .await;
            (result, mock)
        });
        // Auto-advance virtual time so the backoff sleeps complete instantly.
        // Loop a few times; total of 6s of virtual time covers 2s + 4s backoffs.
        for _ in 0..10 {
            tokio::time::advance(std::time::Duration::from_secs(2)).await;
            tokio::task::yield_now().await;
        }
        let (result, mock) = handle.await.unwrap();

        assert!(
            result.is_ok(),
            "expected elevation to succeed, got {result:?}"
        );
        let total = mock.calls.load(Ordering::SeqCst);
        // 3 base-model attempts (initial + 2 retries) + 1 elevated retry = 4
        assert_eq!(total, 4, "expected 3 base + 1 elevated call, got {total}");

        let seen = mock.seen_models.lock().unwrap();
        assert_eq!(
            seen.len(),
            4,
            "expected 4 recorded model overrides, got {}",
            seen.len()
        );
        // First three calls: no override (base model).
        assert!(seen[0].is_none() && seen[1].is_none() && seen[2].is_none());
        // Final call: elevated.
        assert_eq!(seen[3].as_deref(), Some("claude-opus-4-6"));
    }

    /// #222: When `assignments_dir` and `code_target` diverge, the
    /// reconcile step must read the manifest from `assignments_dir` and
    /// move misrouted files into `code_target` (the user's project tree),
    /// not back into `assignments_dir`.
    ///
    /// Why: This locks in the invariant that `--out-dir` (artifacts) and
    /// `--project-dir` (code) stay separated. A regression that confused
    /// the two would silently put generated source files back under
    /// `out/` — exactly the bug #222 tracks.
    /// What: Plants a divergent layout (project_root, code_target,
    /// assignments_dir all distinct), with one file misrouted at
    /// project_root, and asserts the reconciler relocates it to
    /// `code_target`, NOT `assignments_dir`.
    /// Test: This test.
    #[tokio::test]
    async fn reconcile_code_outputs_against_divergent_dirs() {
        // Note: `reconcile_code_outputs_against` reads CWD as project_root.
        // To keep the test deterministic we exercise the divergence by
        // writing the stray file at the real CWD's relative location and
        // then cleaning up. This mirrors what the wave-loop sees in
        // production where claude-code anchors writes at the git repo root.
        let tmp = tempfile::tempdir().unwrap();
        let assignments_dir = tmp.path().join("artifacts");
        let code_target = tmp.path().join("project");
        tokio::fs::create_dir_all(&assignments_dir).await.unwrap();
        tokio::fs::create_dir_all(&code_target).await.unwrap();

        // Sanity: divergent paths.
        assert_ne!(assignments_dir, code_target);

        // assignments.json with one declared file under a uniquely-named
        // subdirectory so this test can't collide with anything actually
        // present in the test runner's CWD.
        let unique = format!("oss_222_test_{}", uuid::Uuid::new_v4().simple());
        let rel_path = format!("{unique}/foo.py");
        let asg_body = format!(
            r#"{{"error_convention":"exceptions","waves":[{{"wave":1,"files":[{{"path":"{rel_path}","stub":null,"purpose":"test","depends_on":[],"max_lines":100}}]}}]}}"#,
            rel_path = rel_path
        );
        tokio::fs::write(assignments_dir.join("assignments.json"), asg_body)
            .await
            .unwrap();

        // Plant the misrouted file at the real CWD (= project_root for
        // `reconcile_code_outputs_against`). Use a unique subdirectory so
        // the test is isolated; clean up regardless of outcome.
        let cwd = std::env::current_dir().unwrap();
        let stray = cwd.join(&rel_path);
        tokio::fs::create_dir_all(stray.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&stray, b"# misrouted").await.unwrap();

        // Act.
        let result = reconcile_code_outputs_against(&assignments_dir, &code_target).await;

        // Cleanup unique subdir at CWD regardless of outcome.
        let cleanup_dir = cwd.join(&unique);
        let _ = tokio::fs::remove_dir_all(&cleanup_dir).await;

        result.expect("reconciliation should succeed");

        // Assert: file landed in `code_target`, NOT `assignments_dir`.
        let code_path = code_target.join(&rel_path);
        let artifacts_path = assignments_dir.join(&rel_path);
        assert!(
            code_path.is_file(),
            "#222: file should be in code_target (project dir), got missing at {}",
            code_path.display()
        );
        assert!(
            !artifacts_path.exists(),
            "#222: file must NOT land in assignments_dir (out_dir / artifacts)"
        );
    }
}
