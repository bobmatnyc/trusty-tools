//! `WorkflowEngine` — iterates phases, runs each agent, threads outputs.
//!
//! Why: The prescriptive Research -> Plan -> Code -> QA -> Observe flow
//! needs a deterministic driver that feeds each phase's output into the
//! next via templates. An explicit engine keeps that orchestration logic
//! out of `main.rs` and behind a testable seam (`AgentRunner`).
//! What: This module owns the `WorkflowEngine` type, its builder surface, and
//! the pre-plan skill-discovery helper. The phase-loop driver
//! (`run_with_perf_and_dirs`) lives in `run`, prompt assembly in `prompt`, and
//! post-loop finalization in `finalize` — split out (#359) to keep each file
//! under the 500-line cap while preserving the public surface.
//! Test: With a mock `AgentRunner` that returns fixed outputs per agent,
//! `run()` should produce a populated context and invoke the runner once
//! per phase. Sub-pieces (config parsing, context templating) are unit-tested
//! and the engine end-to-end behavior is covered in the `tests` submodule.

use std::path::PathBuf;
use std::sync::Arc;

use crate::context::HistoryIndexer;
use crate::init::InitContext;
use crate::inspection::task_signals::TaskSignals;
use crate::session::SessionManager;
use crate::skills::registry::SkillRegistry as TagSkillRegistry;
use crate::skills::{SkillRegistry, SkillsLoader};
use crate::tools::traits::AgentRunner;
use crate::workflow::context::WorkflowContext;
use crate::workflow::error::WorkflowError;
use crate::workflow::tickets::TicketManager;

use super::skills::skill_summary_for;

mod dispatch;
mod finalize;
mod post_phase;
mod prompt;
mod run;
mod setup;

#[cfg(test)]
mod tests;

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
    /// #118: User-scoped memory prompt suffix sourced from `~/.trusty-agents/memory/`.
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
/// Test: `skill_discovery_extracts_python_fastapi_tags`,
/// `skill_discovery_returns_top_n_by_effectiveness`.
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
    /// Test: `skill_discovery_extracts_python_fastapi_tags`.
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
    /// Test: `skill_discovery_extracts_python_fastapi_tags`,
    /// `skill_discovery_returns_top_n_by_effectiveness`.
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
    /// Why (#222): When a user runs `trusty-agents --project-dir <existing-project>`
    /// they want generated source files to land in their project tree, while
    /// workflow artifacts (`assignments.json`, `workflow-report.md`, perf
    /// records, stubs) still live under `out_dir`. Threading two paths keeps
    /// artifacts colocated for inspection without polluting the user's project.
    /// What: Identical to `run_with_perf` but accepts a separate `code_dir`.
    /// When `code_dir` is `None`, falls back to `out_dir` (legacy behavior —
    /// generated code lands alongside artifacts).
    /// Test: `--project-dir . --out-dir /tmp/x` writes source files to CWD and
    /// artifacts to /tmp/x; covered in the engine integration tests.
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
}
