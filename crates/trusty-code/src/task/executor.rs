//! Daemon-owned background task execution (#2056): wires the session/event
//! layer (#2054/#2055) to the existing engine (`agent_loop`/`runner`/`tools`/
//! `llm`) — the M1 control-plane cut line.
//!
//! Why: `run_task` (the CLI's `tcode run-task`) already proves this engine
//! end-to-end, but it runs synchronously, in the calling process, and
//! renders a `RunReport` for a terminal. A daemon-driven task must instead
//! run as a background task the triggering RPC does NOT block on, stream
//! its progress live to `session.attach`ed clients via the #2055 event
//! envelope, and persist its outcome onto the `Session` for a future
//! `session.get_transcript` (#2058). This module is new glue, NOT a fork of
//! the engine: it builds the exact same PM-delegates-to-engineer shape
//! `run_task::execute_run_task` does (see that module's doc comment), reusing
//! `agent_loop::AgentLoop`, `runner::InProcessAgentRunner`, `tools::*`, and
//! `run_task`'s own public `RecordingLlmClient`/`TurnRecord`/`aggregate_usage`
//! machinery — only the orchestration shell (spawn-and-report vs.
//! run-and-render) differs.
//! What: [`TaskRunParams`] carries one run's inputs; [`spawn_task_run`] is
//! the single entry point `task::protocol::task_run` calls — it reserves the
//! session's execution slot synchronously (rejecting a second overlapping
//! run before any background work starts), then hands the actual run off to
//! a `tokio::spawn`'d task that builds both loops (attaching the #2056
//! `SessionToolEventSink` + a shared cancel flag to BOTH the PM's and the
//! delegated engineer's `AgentLoop`), drives the PM loop, prices the
//! transcript PER ROLE against each role's own resolved model (avoiding the
//! #1475 single-model mispricing this ticket was warned not to reintroduce),
//! persists the outcome, and transitions the session to its terminal state.
//! Test: `task::executor::tests::*`; the full flow end-to-end (a real
//! subprocess) in `tests/task_e2e.rs`.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;

use crate::agent_loop::{AgentLoop, AgentLoopConfig, ToolEventSink};
use crate::agents::AgentConfig;
use crate::jsonrpc::RpcError;
use crate::llm::LlmClientTrait;
use crate::project_context::load_project_context;
use crate::prompt::assemble_system_prompt;
use crate::provider::resolve_model;
use crate::run_task::{RecordingLlmClient, SharedTranscript, TurnRecord};
use crate::runner::{InProcessAgentRunner, RegistryFactory};
use crate::session::{SessionRegistry, SessionStatus};
use crate::tools::{
    AgentOutput, AgentRunner, BashTool, DelegateToAgentTool, EditTool, ReadFileTool, RunContext,
    ToolRegistry, WriteFileTool,
};

use super::sink::SessionToolEventSink;

/// Default bash timeout for the engineer's tools, in seconds (mirrors
/// `run_task::ENGINEER_BASH_TIMEOUT_SECS` — not exported, so duplicated as a
/// small constant rather than restructuring that module's visibility).
const ENGINEER_BASH_TIMEOUT_SECS: u64 = 120;

/// Hardcoded delegated engineer agent name, matching the `run_task`/fixture
/// convention used across this crate (`<agents_dir>/python-engineer.toml`).
/// A future ticket may make this configurable per `task.run` call.
pub const ENGINEER_AGENT_NAME: &str = "python-engineer";

/// Inputs to start one background task execution.
///
/// Why: bundles everything `spawn_task_run` needs so `task::protocol`'s
/// handler stays a thin params-parsing shim.
/// What: `session_id` must already exist in the registry (created by the
/// caller — see `task::protocol::task_run`); `agent_name` is the top-level
/// (PM) agent, `task` is the free-form request text, `project`/`agents_dir`
/// mirror `run_task::RunTaskParams`, and `model_override` pins the
/// DELEGATED ENGINEER's model for this run only (mirrors `run_task`'s
/// `--engineer-model`).
#[derive(Debug, Clone)]
pub struct TaskRunParams {
    pub session_id: String,
    pub task: String,
    pub agent_name: String,
    pub project: PathBuf,
    pub agents_dir: PathBuf,
    pub model_override: Option<String>,
}

/// Reserve the session's execution slot and spawn the background run.
///
/// Why: the single entry point `task::protocol::task_run` calls. Reserving
/// the slot SYNCHRONOUSLY (before any `tokio::spawn`) is what makes "a
/// second overlapping `task.run` on the same session is rejected"
/// observable to the CALLER of the second request, not just internally.
/// What: calls `registry.begin_execution` (propagating `session_not_found`/
/// `invalid_argument` — already terminal / already running — verbatim),
/// then spawns [`run_and_record`] and immediately attaches the real
/// `JoinHandle` back onto the registry (see `SessionRegistry::attach_execution_handle`'s
/// docs for why that is a separate call).
/// Test: `task::executor::tests::spawn_task_run_rejects_second_overlapping_run`;
/// the full run is exercised by `tests/task_e2e.rs`.
pub fn spawn_task_run(
    registry: Arc<SessionRegistry>,
    llm: Arc<dyn LlmClientTrait>,
    params: TaskRunParams,
) -> Result<(), RpcError> {
    let cancel = registry.begin_execution(&params.session_id)?;
    let session_id = params.session_id.clone();
    let registry_for_task = Arc::clone(&registry);

    let handle = tokio::spawn(async move {
        run_and_record(registry_for_task, llm, params, cancel).await;
    });
    registry.attach_execution_handle(&session_id, handle);
    Ok(())
}

/// The actual PM -> engineer run, executed entirely inside the spawned task.
///
/// Why: kept as one function (rather than splitting further) because every
/// step depends on state built by the previous one; the doc comment on
/// `spawn_task_run` and the module docs carry the "why this shape" framing.
/// What: loads the PM config (a failure here is recorded as a `Failed`
/// outcome, not a panic); builds the engineer runner + PM registry with the
/// SAME shape `run_task::execute_run_task` uses; runs the PM `AgentLoop`
/// (both it and the delegated engineer share the #2056 sink + cancel flag);
/// prices the transcript per-role; persists the outcome; and transitions the
/// session to `Finished`/`Cancelled`/`Failed` before clearing the execution
/// slot.
async fn run_and_record(
    registry: Arc<SessionRegistry>,
    llm: Arc<dyn LlmClientTrait>,
    params: TaskRunParams,
    cancel: Arc<AtomicBool>,
) {
    let session_id = params.session_id.clone();
    let transcript: SharedTranscript = Arc::new(Mutex::new(Vec::new()));
    let sink: Arc<dyn ToolEventSink> = Arc::new(SessionToolEventSink::new(
        Arc::clone(&registry),
        session_id.clone(),
    ));

    let pm_config_path = params
        .agents_dir
        .join(format!("{}.toml", params.agent_name));
    let pm_config = match AgentConfig::load(&pm_config_path) {
        Ok(cfg) => cfg,
        Err(e) => {
            finish_with_failure(&registry, &session_id, &format!("PM config error: {e:#}")).await;
            return;
        }
    };

    let project_context = load_project_context(&params.project);
    let pm_model = resolve_model(&pm_config, None);
    let engineer_model = resolve_engineer_model(&params);

    let engineer_runner = build_engineer_runner(
        Arc::clone(&llm),
        &params,
        project_context.clone(),
        Arc::clone(&transcript),
        Arc::clone(&sink),
        Arc::clone(&cancel),
    );

    let mut pm_registry = ToolRegistry::new();
    pm_registry.register(Arc::new(
        DelegateToAgentTool::new(engineer_runner).with_config_dir(params.agents_dir.clone()),
    ));

    let pm_llm: Arc<dyn LlmClientTrait> = Arc::new(RecordingLlmClient::new(
        Arc::clone(&llm),
        "pm",
        Arc::clone(&transcript),
    ));

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
    )
    .with_tool_event_sink(Arc::clone(&sink))
    .with_cancel_flag(Arc::clone(&cancel));

    let result = pm_loop.run(&pm_system, &params.task).await;

    let turns = transcript.lock().map(|g| g.clone()).unwrap_or_default();
    let (usage, cost) = aggregate_usage_per_role(&turns, &pm_model, &engineer_model);
    registry.set_run_outcome(&session_id, turns, usage, Some(cost));

    let terminal_status = match result {
        Ok(_output) => SessionStatus::Finished,
        Err(crate::agent_loop::AgentLoopError::Cancelled { .. }) => SessionStatus::Cancelled,
        Err(e) => {
            let _ = registry.record_log(&session_id, "error", &format!("run failed: {e}"));
            SessionStatus::Failed
        }
    };
    let _ = registry.finish(&session_id, terminal_status);
    registry.finish_execution(&session_id);
}

/// Build the in-process engineer runner with project-scoped fs/bash tools,
/// the #2056 sink, and the shared cancel flag (mirrors
/// `run_task::build_engineer_runner`, which is private to that module).
fn build_engineer_runner(
    llm: Arc<dyn LlmClientTrait>,
    params: &TaskRunParams,
    project_context: Option<String>,
    transcript: SharedTranscript,
    sink: Arc<dyn ToolEventSink>,
    cancel: Arc<AtomicBool>,
) -> Arc<dyn AgentRunner> {
    let engineer_llm: Arc<dyn LlmClientTrait> = Arc::new(RecordingLlmClient::new(
        llm,
        ENGINEER_AGENT_NAME,
        transcript,
    ));

    let factory: Arc<dyn RegistryFactory> = Arc::new(ProjectToolFactory {
        project: params.project.clone(),
    });

    let mut runner = InProcessAgentRunner::new(engineer_llm, factory, params.agents_dir.clone())
        .with_tool_event_sink(sink)
        .with_cancel_flag(cancel);
    if let Some(ctx) = project_context {
        runner = runner.with_project_context(ctx);
    }
    apply_engineer_model_override(Arc::new(runner), params)
}

/// Builds the engineer's project-scoped tool registry for each delegation
/// (mirrors `run_task::ProjectToolFactory`, which is private to that module).
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

/// Apply the per-run engineer-model override, if any (mirrors
/// `run_task::apply_engineer_model_override` / `ModelPinningRunner`, both
/// private to that module).
fn apply_engineer_model_override(
    runner: Arc<dyn AgentRunner>,
    params: &TaskRunParams,
) -> Arc<dyn AgentRunner> {
    match &params.model_override {
        Some(model) => Arc::new(ModelPinningRunner {
            inner: runner,
            model: model.clone(),
        }),
        None => runner,
    }
}

/// An `AgentRunner` decorator pinning a fixed model for every delegation
/// (mirrors `run_task::ModelPinningRunner`).
struct ModelPinningRunner {
    inner: Arc<dyn AgentRunner>,
    model: String,
}

#[async_trait]
impl AgentRunner for ModelPinningRunner {
    async fn run(&self, agent_name: &str, task: &str) -> anyhow::Result<AgentOutput> {
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
    ) -> anyhow::Result<AgentOutput> {
        let mut ctx = ctx.clone();
        ctx.model = Some(self.model.clone());
        self.inner.run_with_context(agent_name, task, &ctx).await
    }
}

/// Resolve the engineer's model the SAME way `InProcessAgentRunner` does
/// internally, purely so the cost split below can price the engineer's
/// turns against its own slug rather than blending everything under the PM
/// model (the #1475 mispricing pattern this ticket was warned not to
/// reintroduce).
///
/// Why: `InProcessAgentRunner` resolves the engineer's model internally and
/// does not expose it; loading the same config file here is cheap and keeps
/// `run_and_record`'s pricing step independently correct without changing
/// the runner's public surface.
/// What: loads `<agents_dir>/python-engineer.toml`; on any load failure
/// (missing/invalid file) falls back to the literal string `"unknown"` —
/// `crate::perf::cost_usd` degrades gracefully (Sonnet-equivalent pricing)
/// for an unrecognised model rather than erroring, so a missing engineer
/// config degrades pricing accuracy, not the whole run.
fn resolve_engineer_model(params: &TaskRunParams) -> String {
    let path = params
        .agents_dir
        .join(format!("{ENGINEER_AGENT_NAME}.toml"));
    let Ok(engineer_config) = AgentConfig::load(&path) else {
        return "unknown".to_string();
    };
    match &params.model_override {
        Some(model) => {
            let ctx = RunContext {
                model: Some(model.clone()),
                ..Default::default()
            };
            resolve_model(&engineer_config, Some(&ctx))
        }
        None => resolve_model(&engineer_config, None),
    }
}

/// Aggregate transcript usage, pricing EACH turn against its OWN role's
/// resolved model rather than blending the whole transcript under one model
/// (the #1475 concern; a full fix is #2061 — this is a narrow, independently
/// correct improvement scoped to this new code path only, not a rewrite of
/// `run_task::aggregate_usage`, which is left untouched for its own callers).
///
/// Why: `run_task::aggregate_usage` prices the WHOLE transcript against a
/// single model (the PM's), which mis-prices every engineer turn whenever
/// the engineer routes to a different model — exactly what #1475 flags.
/// Since this is new code, there is no reason to copy that known bug forward.
/// What: sums `TokenUsage` across every turn (unchanged), but computes cost
/// per turn using `pm_model` for `role == "pm"` and `engineer_model`
/// otherwise, then sums the per-turn costs.
/// Test: `task::executor::tests::aggregate_usage_per_role_prices_each_role_separately`.
fn aggregate_usage_per_role(
    turns: &[TurnRecord],
    pm_model: &str,
    engineer_model: &str,
) -> (crate::perf::TokenUsage, f64) {
    let mut total = crate::perf::TokenUsage::default();
    let mut cost = 0.0;
    for turn in turns {
        total.add(&turn.usage);
        let model = if turn.role == "pm" {
            pm_model
        } else {
            engineer_model
        };
        cost += crate::perf::cost_usd(
            model,
            turn.usage.prompt_tokens,
            turn.usage.completion_tokens,
            turn.usage.cache_read_tokens,
            turn.usage.cache_creation_tokens,
        );
    }
    (total, cost)
}

/// Record a diagnostic log line and transition the session to `Failed`.
///
/// Why: a PM-config load failure happens before any loop runs, so there is
/// no transcript/usage to persist — just the terminal state and a reason an
/// operator (or a future `session.get_transcript`) can see.
async fn finish_with_failure(registry: &SessionRegistry, session_id: &str, message: &str) {
    let _ = registry.record_log(session_id, "error", message);
    let _ = registry.finish(session_id, SessionStatus::Failed);
    registry.finish_execution(session_id);
}

#[cfg(test)]
#[path = "executor_tests.rs"]
mod tests;
