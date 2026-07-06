//! `tcode run-task` end-to-end execution (#1034, #1035).
//!
//! Why: This is the closer that makes the `tcode` binary actually run a task. It
//! wires every M1 piece together: load the project `CLAUDE.md` context (#1033),
//! assemble the PM system prompt, run the PM through an `AgentLoop` against the
//! real (or mocked) `LlmClient`, let the PM delegate to the `python-engineer` via
//! the `DelegateToAgentTool` + `InProcessAgentRunner`, and have the engineer run
//! its own loop with fs/bash tools scoped to the project. The run captures a diff
//! of the working tree, the PM+engineer transcript, and aggregated token/cost
//! usage, then renders both a human and a JSON report with meaningful exit codes.
//! What: `RunTaskParams` carries the CLI inputs; `execute_run_task` is the async
//! orchestrator returning a `RunReport`. The LLM client is injected so production
//! passes a real `LlmClient` and tests pass a scripted mock — no live key needed.
//! Test: `run_task::tests` drive the whole PM→engineer path offline.

mod diff;
mod recorder;
mod report;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;

use crate::agent_loop::{AgentLoop, AgentLoopConfig};
use crate::agents::AgentConfig;
use crate::llm::LlmClientTrait;
use crate::project_context::load_project_context;
use crate::prompt::assemble_system_prompt;
use crate::provider::resolve_model;
use crate::runner::{InProcessAgentRunner, RegistryFactory};
use crate::tools::{
    AgentOutput, AgentRunner, BashTool, DelegateToAgentTool, EditTool, ReadFileTool, RunContext,
    ToolRegistry, WriteFileTool,
};

pub use recorder::{RecordingLlmClient, SharedTranscript, TurnRecord};
pub use report::{ExitCode, RunReport, aggregate_usage_per_role};

/// Default bash timeout for the engineer's tools, in seconds.
const ENGINEER_BASH_TIMEOUT_SECS: u64 = 120;

/// The delegated engineer's agent name (matches
/// `task::executor::ENGINEER_AGENT_NAME`, the daemon path's own copy of this
/// same literal — kept as two constants, not one shared item, since neither
/// module depends on the other and a shared constant would be a heavier
/// coupling than the two paths' otherwise-independent module boundaries
/// warrant).
const ENGINEER_AGENT_NAME: &str = "python-engineer";

/// Inputs to a single `run-task` invocation, parsed from the CLI.
///
/// Why: Bundling the inputs keeps `execute_run_task`'s signature stable as flags
/// are added and lets the binary layer construct one value from clap.
/// What: `agent` is the top-level (PM) agent name; `task` is the request; `project`
/// is the canonical project root; `agents_dir` is where `<agent>.toml` live;
/// `engineer_model` is the optional per-run engineer model override (#1035).
/// Test: `run_task::tests` build these directly.
#[derive(Debug, Clone)]
pub struct RunTaskParams {
    /// Top-level agent name (typically `"pm"`).
    pub agent: String,
    /// Free-form task description.
    pub task: String,
    /// Canonical project root.
    pub project: PathBuf,
    /// Directory holding `<agent>.toml` configs.
    pub agents_dir: PathBuf,
    /// Optional per-run engineer model override (#1035). `None` routes the
    /// engineer via its own config model.
    pub engineer_model: Option<String>,
}

/// Execute a `run-task` end-to-end and return the rendered report.
///
/// Why: The single orchestration entry point the binary calls (with a real
/// `LlmClient`) and tests call (with a scripted mock). Keeping the LLM client as
/// an injected `Arc<dyn LlmClientTrait>` is what makes the whole path testable
/// offline without a network call or an API key.
/// What: Loads the PM config, assembles its prompt (BASE + PM prompt + project
/// `CLAUDE.md`), builds the engineer runner (fs/bash scoped to the project, with
/// the optional per-run model override), wraps it in the PM's
/// `DelegateToAgentTool`, snapshots the working tree, runs the PM `AgentLoop`,
/// snapshots again, and assembles a `RunReport` (diff + transcript + usage/cost +
/// exit code). A PM-config or loop error yields a `ConfigError`/`RunFailure`
/// report rather than a panic.
/// Test: `run_task::tests::end_to_end_pm_delegates_to_engineer`,
/// `diff_reflects_engineer_file_change`, `usage_and_cost_aggregate_end_to_end`,
/// `exit_code_reflects_run_failure`.
pub async fn execute_run_task(params: RunTaskParams, llm: Arc<dyn LlmClientTrait>) -> RunReport {
    let transcript: SharedTranscript = Arc::new(Mutex::new(Vec::new()));

    // Load the PM config; a missing/invalid config is a configuration error.
    let pm_config =
        match AgentConfig::load(&params.agents_dir.join(format!("{}.toml", params.agent))) {
            Ok(cfg) => cfg,
            Err(e) => return config_error_report(&params, &transcript, &format!("{e:#}")),
        };

    // Load project context (#1033) — absent file is fine.
    let project_context = load_project_context(&params.project);

    // Resolve the PM's own model (no per-call override for the PM).
    let pm_model = resolve_model(&pm_config, None);

    // Build the engineer runner, scoped to the project working dir, sharing the
    // transcript (engineer turns are tagged "python-engineer").
    let engineer_runner = build_engineer_runner(
        Arc::clone(&llm),
        &params,
        project_context.clone(),
        Arc::clone(&transcript),
    );

    // The PM's tool registry: just the delegate tool (the PM orchestrates; the
    // engineer does the file work). Pre-flight validation uses the agents dir.
    let mut pm_registry = ToolRegistry::new();
    pm_registry.register(Arc::new(
        DelegateToAgentTool::new(engineer_runner).with_config_dir(params.agents_dir.clone()),
    ));

    // The PM's loop uses a transcript-recording client tagged "pm".
    let pm_llm: Arc<dyn LlmClientTrait> = Arc::new(RecordingLlmClient::new(
        Arc::clone(&llm),
        "pm",
        Arc::clone(&transcript),
    ));

    // Inject DOC-28 catch-up digest as seed context into the PM prompt (#1762 PR2).
    // The catch-up engine is async and fail-open: if the daemon is offline or the
    // project has no activity, it returns None and the prompt is assembled without
    // the section.  Sub-agents are NOT wired here; only the PM receives this digest.
    let catchup_ctx = crate::catchup::pm_catchup_context(&params.project).await;

    let pm_system = assemble_system_prompt(
        &pm_config,
        project_context.as_deref(),
        catchup_ctx.as_deref(),
    );
    let pm_loop = AgentLoop::new(
        AgentLoopConfig {
            model: pm_model.clone(),
            ..AgentLoopConfig::default()
        },
        pm_llm,
        Arc::new(pm_registry),
    );

    // Snapshot before, run the PM, snapshot after.
    let before = diff::capture_snapshot(&params.project);
    let pm_result = pm_loop.run(&pm_system, &params.task).await;
    let after = diff::capture_snapshot(&params.project);

    // Resolve the engineer's model independently (#1475 bug 1) so the report
    // prices its turns at its OWN rate rather than blending everything
    // under the PM model.
    let engineer_model = resolve_agent_model_slug(
        &params.agents_dir,
        ENGINEER_AGENT_NAME,
        params.engineer_model.as_deref(),
    );

    assemble_report(
        &params,
        &transcript,
        &pm_model,
        &engineer_model,
        before,
        after,
        pm_result,
    )
}

/// Build the in-process engineer runner with project-scoped fs/bash tools.
///
/// Why: The engineer must write/read/run inside the project and see the same
/// project context as the PM. This wires a `RegistryFactory` that constructs fs +
/// bash tools scoped to the project dir plus the project context, then routes the
/// result through the per-run model-override seam (`apply_engineer_model_override`).
/// What: Returns an `Arc<dyn AgentRunner>` the `DelegateToAgentTool` dispatches
/// to. The engineer's own LLM turns are recorded under the "python-engineer" role.
/// Test: `run_task::tests::end_to_end_pm_delegates_to_engineer`.
fn build_engineer_runner(
    llm: Arc<dyn LlmClientTrait>,
    params: &RunTaskParams,
    project_context: Option<String>,
    transcript: SharedTranscript,
) -> Arc<dyn AgentRunner> {
    let engineer_llm: Arc<dyn LlmClientTrait> = Arc::new(RecordingLlmClient::new(
        llm,
        ENGINEER_AGENT_NAME,
        transcript,
    ));

    let factory: Arc<dyn RegistryFactory> = Arc::new(ProjectToolFactory {
        project: params.project.clone(),
    });

    let mut runner = InProcessAgentRunner::new(engineer_llm, factory, params.agents_dir.clone());
    if let Some(ctx) = project_context {
        runner = runner.with_project_context(ctx);
    }
    apply_engineer_model_override(Arc::new(runner), params)
}

/// Builds the engineer's project-scoped tool registry for each delegation.
///
/// Why: The engineer needs real file and shell tools that cannot escape the
/// project root. This factory constructs `read_file`, `write_file`, `edit`, and
/// `bash` all scoped to `self.project` so the engineer operates only inside the
/// project working tree.
/// What: Implements `RegistryFactory::build`, returning an `Arc<ToolRegistry>`
/// with the four tools; the runner then gates them by the engineer's
/// `tools.allowed`.
/// Test: `run_task::tests::end_to_end_pm_delegates_to_engineer` (the engineer's
/// `write_file` actually writes into the project).
struct ProjectToolFactory {
    project: PathBuf,
}

#[async_trait]
impl RegistryFactory for ProjectToolFactory {
    async fn build(&self, _agent: &AgentConfig, _ctx: &RunContext) -> Arc<ToolRegistry> {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(ReadFileTool::new(&self.project)));
        reg.register(Arc::new(WriteFileTool::new(&self.project)));
        reg.register(Arc::new(EditTool::new(&self.project)));
        reg.register(Arc::new(BashTool::new(
            Some(self.project.clone()),
            Duration::from_secs(ENGINEER_BASH_TIMEOUT_SECS),
        )));
        Arc::new(reg)
    }
}

/// Apply the per-run engineer-model override to the runner, if any (#1035).
///
/// Why: The engineer routes to its own configured model by default. A per-run
/// `--engineer-model` (or `TCODE_ENGINEER_MODEL`) must reroute *only* the engineer
/// for that single run — the model-comparison harness varies the engineer model
/// while the PM model stays fixed. Keeping the override here lets
/// `build_engineer_runner` stay agnostic to whether a model is pinned.
/// What: When `params.engineer_model` is `Some`, wraps the runner in a
/// `ModelPinningRunner` that injects `RunContext.model` on every delegation;
/// otherwise returns the runner unchanged.
/// Test: `run_task::tests::engineer_model_swap_routes_engineer`,
/// `two_runs_route_engineer_to_distinct_slugs`.
fn apply_engineer_model_override(
    runner: Arc<dyn AgentRunner>,
    params: &RunTaskParams,
) -> Arc<dyn AgentRunner> {
    match &params.engineer_model {
        Some(model) => Arc::new(ModelPinningRunner {
            inner: runner,
            model: model.clone(),
        }),
        None => runner,
    }
}

/// Resolve an agent's model slug, honouring an optional per-run override
/// (#1035, #1475 bug 1 fix's prerequisite).
///
/// Why: `InProcessAgentRunner` resolves the engineer's model internally and
/// does not expose it, so pricing the engineer's turns against its OWN
/// model (rather than blending everything under the PM model — the #1475
/// bug) needs this same resolution done independently, here, purely for the
/// pricing step. Shared with the daemon path (`task::executor`'s own
/// `resolve_engineer_model`, which now delegates here) so the two paths
/// can never independently drift on how per-role pricing resolves a model.
/// What: loads `<agents_dir>/<agent_name>.toml`; on any load failure
/// (missing/invalid file) falls back to the literal string `"unknown"` —
/// `crate::perf::cost_usd` degrades gracefully (Sonnet-equivalent pricing)
/// for an unrecognised model rather than erroring, so a missing agent
/// config degrades pricing accuracy, not the whole run. When
/// `model_override` is `Some`, it wins over the config's own model (mirrors
/// `RunContext`'s override precedence).
/// Test: `run_task::tests::resolve_agent_model_slug_falls_back_when_config_missing`,
/// `run_task::tests::resolve_agent_model_slug_honours_override`.
pub fn resolve_agent_model_slug(
    agents_dir: &std::path::Path,
    agent_name: &str,
    model_override: Option<&str>,
) -> String {
    let path = agents_dir.join(format!("{agent_name}.toml"));
    let Ok(config) = AgentConfig::load(&path) else {
        return "unknown".to_string();
    };
    match model_override {
        Some(model) => {
            let ctx = RunContext {
                model: Some(model.to_string()),
                ..Default::default()
            };
            resolve_model(&config, Some(&ctx))
        }
        None => resolve_model(&config, None),
    }
}

/// An `AgentRunner` decorator that pins a fixed model for every delegation (#1035).
///
/// Why: `DelegateToAgentTool::execute` calls `runner.run` with a default
/// `RunContext`, so the per-run `--engineer-model` override cannot be threaded
/// through the tool itself. This decorator injects `RunContext.model` and forwards
/// to `run_with_context`, so the engineer's loop routes to the pinned slug while
/// the PM model stays fixed.
/// What: Holds the inner runner and the pinned model; both `run` and
/// `run_with_context` set `ctx.model` before delegating.
/// Test: `run_task::tests::engineer_model_swap_routes_engineer`.
struct ModelPinningRunner {
    inner: Arc<dyn AgentRunner>,
    model: String,
}

#[async_trait]
impl AgentRunner for ModelPinningRunner {
    async fn run(&self, agent_name: &str, task: &str) -> Result<AgentOutput> {
        let ctx = RunContext {
            model: Some(self.model.clone()),
            ..Default::default()
        };
        self.inner.run_with_context(agent_name, task, &ctx).await
    }

    async fn run_with_context(
        &self,
        agent_name: &str,
        task: &str,
        ctx: &RunContext,
    ) -> Result<AgentOutput> {
        let mut ctx = ctx.clone();
        ctx.model = Some(self.model.clone());
        self.inner.run_with_context(agent_name, task, &ctx).await
    }
}

/// Drain the shared transcript into an owned `Vec`.
///
/// Why: The report owns its transcript; this lifts it out of the shared mutex.
/// What: Clones the locked vector (empty on a poisoned lock — the run must not
/// panic while building the report).
/// Test: Exercised by every end-to-end test.
fn drain_transcript(transcript: &SharedTranscript) -> Vec<TurnRecord> {
    transcript.lock().map(|g| g.clone()).unwrap_or_default()
}

/// Build a `ConfigError` report (PM config missing/invalid).
///
/// Why: A configuration failure must produce a faithful report + exit code, not a
/// panic, so callers can branch on it.
/// What: Returns a `RunReport` with an empty diff, the (likely empty) transcript,
/// zero usage, and `ExitCode::ConfigError`; the error text rides in the task line
/// of the human form via a synthesized note.
/// Test: `run_task::tests::missing_pm_config_is_config_error`.
fn config_error_report(
    params: &RunTaskParams,
    transcript: &SharedTranscript,
    err: &str,
) -> RunReport {
    RunReport {
        agent: params.agent.clone(),
        task: format!("{} [config error: {err}]", params.task),
        diff: String::new(),
        transcript: drain_transcript(transcript),
        usage: crate::perf::TokenUsage::default(),
        cost_usd: None,
        exit: ExitCode::ConfigError,
    }
}

/// Assemble the final report from the run's outcome.
///
/// Why: Centralises the mapping from (PM loop result, before/after snapshots) to a
/// `RunReport` with the correct exit code so the success / no-change / failure
/// branches never drift.
/// What: On a PM-loop error → `RunFailure` (with whatever transcript accrued). On
/// success → compute the diff; empty diff → `NoChanges`, else `Success`. Usage and
/// cost are aggregated from the transcript and priced PER ROLE — `pm_model` for
/// `"pm"` turns, `engineer_model` for the delegated engineer's — per the #1475
/// bug 1 fix (`aggregate_usage_per_role`), not blended under one model.
/// Test: `run_task::tests::*` (success, no-change, and failure paths).
fn assemble_report(
    params: &RunTaskParams,
    transcript: &SharedTranscript,
    pm_model: &str,
    engineer_model: &str,
    before: diff::Snapshot,
    after: diff::Snapshot,
    pm_result: Result<AgentOutput, crate::agent_loop::AgentLoopError>,
) -> RunReport {
    let turns = drain_transcript(transcript);

    // A PM-loop error is a runtime failure; the transcript still reflects whatever
    // turns ran before the error. The PM's `AgentOutput` content is not needed for
    // the report — the transcript and diff are the authoritative artifacts.
    if let Err(e) = pm_result {
        return RunReport {
            agent: params.agent.clone(),
            task: format!("{} [run failure: {e}]", params.task),
            diff: String::new(),
            transcript: turns,
            usage: crate::perf::TokenUsage::default(),
            cost_usd: None,
            exit: ExitCode::RunFailure,
        };
    }

    let rendered_diff = diff::diff_snapshots(&before, &after);

    // Sum usage across every recorded PM + engineer turn, pricing each turn
    // against its OWN role's resolved model (#1475 bug 1 fix). The PM's own
    // `AgentOutput.usage` omits the engineer's tokens (they return as a
    // tool-result string), so the transcript is the faithful total.
    let (total_usage, cost) = aggregate_usage_per_role(&turns, pm_model, engineer_model);

    let exit = if rendered_diff.trim().is_empty() {
        ExitCode::NoChanges
    } else {
        ExitCode::Success
    };

    RunReport {
        agent: params.agent.clone(),
        task: params.task.clone(),
        diff: rendered_diff,
        transcript: turns,
        usage: total_usage,
        cost_usd: Some(cost),
        exit,
    }
}
