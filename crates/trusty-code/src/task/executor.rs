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
//! `run_task`'s own public `RecordingLlmClient`/`TurnRecord`/
//! `aggregate_usage_per_role`/`resolve_agent_model_slug` machinery — only the
//! orchestration shell (spawn-and-report vs. run-and-render) differs.
//! What: [`TaskRunParams`] carries one run's inputs; [`spawn_task_run`] is
//! the single entry point `task::protocol::task_run` calls — it reserves the
//! session's execution slot synchronously (rejecting a second overlapping
//! run before any background work starts), then hands the actual run off to
//! a `tokio::spawn`'d task that builds both loops (attaching the #2056
//! `SessionToolEventSink` + a shared cancel flag to BOTH the PM's and the
//! delegated engineer's `AgentLoop`), drives the PM loop, prices the
//! transcript PER ROLE against each role's own resolved model via
//! `run_task::aggregate_usage_per_role` (#1475 bug 1 — this daemon path and
//! the legacy CLI path now share the ONE implementation, so they can never
//! independently drift), persists the outcome, and transitions the session
//! to its terminal state.
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
use crate::llm::{DebugCaptureSink, LlmClientTrait, wrap_with_debug_capture};
use crate::mode::HarnessMode;
use crate::project_context::load_project_context;
use crate::prompt::assemble_system_prompt_for_mode;
use crate::provider::{resolve_deadline_secs, resolve_max_tokens, resolve_model};
use crate::run_task::{
    RecordingLlmClient, SharedTranscript, aggregate_usage_per_role, resolve_agent_model_slug,
};
use crate::runner::{InProcessAgentRunner, RegistryFactory};
use crate::session::{SessionRegistry, SessionStatus};
use crate::skills::{FsSkillResolver, format_skill_catalog, locate_skills_dir};
use crate::tools::{
    AgentOutput, AgentRunner, BashTool, DelegateToAgentTool, EditTool, FinishTaskTool,
    ReadFileTool, RunContext, SkillResolver, ToolRegistry, UseSkillTool, WriteFileTool,
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
/// `--engineer-model`). `mode` (#2059) is the ALREADY-RESOLVED `HarnessMode`
/// (`task::protocol::task_run` resolves it via `crate::mode::resolve_mode`
/// before constructing this — resolution is a request-parsing concern, not
/// an execution one).
#[derive(Debug, Clone)]
pub struct TaskRunParams {
    pub session_id: String,
    pub task: String,
    pub agent_name: String,
    pub project: PathBuf,
    pub agents_dir: PathBuf,
    pub model_override: Option<String>,
    pub mode: HarnessMode,
    /// Per-run wall-clock deadline override, in seconds (#2207). `None`
    /// falls through to `crate::provider::resolve_deadline_secs`'s env-var
    /// and default tiers — resolved once in `run_and_record` and applied to
    /// BOTH the PM's own loop and the delegated engineer's loop.
    pub deadline_secs: Option<u64>,
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

    // #2264: mirrors `run_task::execute_run_task`'s own resolution — one
    // shared (optional) debug-capture sink for the whole run, so pm and
    // engineer turns land in one globally-ordered JSONL file when
    // `TCODE_DEBUG_TRANSCRIPT` is set, and cost nothing when it is not.
    let debug_sink: Option<Arc<DebugCaptureSink>> = DebugCaptureSink::from_env();

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

    // #2207: resolve the run's wall-clock deadline once, applied to BOTH the
    // PM's own loop (below) and the delegated engineer's loop
    // (`build_engineer_runner`, via `with_timeout_secs`).
    let deadline_secs = resolve_deadline_secs(params.deadline_secs);

    // #2069: discover the (cheap, metadata-only) skill catalog and, in
    // DailyDriver mode only, register `use_skill` so the PM can lazily fetch
    // a skill's full body on demand. Parity never sees the catalog or the
    // tool — see `assemble_system_prompt_for_mode`'s docs.
    let skills_catalog = daily_driver_skills_catalog(&params);

    let engineer_runner = build_engineer_runner(
        wrap_with_debug_capture(Arc::clone(&llm), ENGINEER_AGENT_NAME, debug_sink.as_ref()),
        &params,
        project_context.clone(),
        skills_catalog.as_ref().map(|(catalog, _)| catalog.clone()),
        Arc::clone(&transcript),
        Arc::clone(&sink),
        Arc::clone(&cancel),
    );

    // #2072: `finish_task` gives the PM a structured, schema-validated way to
    // signal completion alongside the delegate tool.
    let mut pm_registry = ToolRegistry::new();
    pm_registry.register(Arc::new(
        DelegateToAgentTool::new(engineer_runner).with_config_dir(params.agents_dir.clone()),
    ));
    pm_registry.register(Arc::new(FinishTaskTool::new()));
    if let Some((_, resolver)) = &skills_catalog {
        pm_registry.register(Arc::new(UseSkillTool::new(Arc::clone(resolver))));
    }

    let pm_llm: Arc<dyn LlmClientTrait> = Arc::new(RecordingLlmClient::new(
        wrap_with_debug_capture(Arc::clone(&llm), "pm", debug_sink.as_ref()),
        "pm",
        Arc::clone(&transcript),
    ));

    let catchup_ctx = crate::catchup::pm_catchup_context(&params.project).await;
    let pm_system = assemble_system_prompt_for_mode(
        params.mode,
        &pm_config,
        project_context.as_deref(),
        catchup_ctx.as_deref(),
        skills_catalog.as_ref().map(|(catalog, _)| catalog.as_str()),
    );

    let pm_loop = AgentLoop::new(
        AgentLoopConfig {
            model: pm_model.clone(),
            max_tokens: resolve_max_tokens(&pm_config),
            mode: params.mode,
            timeout_secs: deadline_secs,
            ..AgentLoopConfig::default()
        },
        pm_llm,
        Arc::new(pm_registry),
    )
    .with_tool_event_sink(Arc::clone(&sink))
    .with_cancel_flag(Arc::clone(&cancel))
    // #2279: mirrors `run_task::execute_run_task`'s own wiring — the PM
    // never calls `bash` itself, so its verify-before-finish gate scans the
    // delegated engineer's turns via the SAME shared `transcript` the
    // engineer's `RecordingLlmClient` records into (`build_engineer_runner`
    // above), rather than the PM's own (bash-less) transcript.
    .with_finish_gate(crate::verify_gate::pm_finish_gate(Arc::clone(&transcript)));

    let result = pm_loop.run(&pm_system, &params.task).await;

    // #2206: usage/cost are aggregated from every turn that DID complete
    // regardless of `result` — this call already ran unconditionally before
    // the terminal-status match below, so a `Failed`/`DeadlineExceeded`
    // outcome still persists real telemetry, not zeroed placeholders.
    let turns = transcript.lock().map(|g| g.clone()).unwrap_or_default();
    let (usage, cost) = aggregate_usage_per_role(&turns, &pm_model, &engineer_model);
    registry.set_run_outcome(&session_id, turns, usage, Some(cost));

    // #2207: a wall-clock deadline is distinct from a genuine run failure —
    // `SessionStatus::DeadlineExceeded` lets a `session.status`/`task.run`
    // consumer tell "timed out, possibly close to done" from "errored".
    let terminal_status = match result {
        Ok(_output) => SessionStatus::Finished,
        Err(crate::agent_loop::AgentLoopError::Cancelled { .. }) => SessionStatus::Cancelled,
        Err(e @ crate::agent_loop::AgentLoopError::Timeout { .. }) => {
            let _ = registry.record_log(&session_id, "error", &format!("run failed: {e}"));
            SessionStatus::DeadlineExceeded
        }
        Err(e) => {
            let _ = registry.record_log(&session_id, "error", &format!("run failed: {e}"));
            SessionStatus::Failed
        }
    };
    let _ = registry.finish(&session_id, terminal_status);
    registry.finish_execution(&session_id);
}

/// Build the in-process engineer runner with project-scoped fs/bash tools,
/// the #2056 sink, the shared cancel flag, and (#2059) the SAME resolved
/// `HarnessMode` as the delegating PM (mirrors `run_task::build_engineer_runner`,
/// which is private to that module).
///
/// `skills_catalog` (#2069) is the same rendered metadata catalog the PM's own
/// prompt gets, threaded through so the delegated engineer's DailyDriver
/// prompt also sees it. Wiring the `use_skill` *tool* into the engineer's own
/// per-delegation registry (`ProjectToolFactory::build`) is deferred — see
/// this crate's #2069 delivery notes. #2207: resolves the SAME wall-clock
/// budget applied to the delegating PM's own loop via
/// `crate::provider::resolve_deadline_secs(params.deadline_secs)` — called
/// again here (rather than threaded in as an extra argument) purely to keep
/// this function's arity under clippy's `too_many_arguments` gate; the
/// resolver is pure given the same `params.deadline_secs`/env state, so a
/// second call is not a correctness risk.
fn build_engineer_runner(
    llm: Arc<dyn LlmClientTrait>,
    params: &TaskRunParams,
    project_context: Option<String>,
    skills_catalog: Option<String>,
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
        mode: params.mode,
    });

    let mut runner = InProcessAgentRunner::new(engineer_llm, factory, params.agents_dir.clone())
        .with_tool_event_sink(sink)
        .with_cancel_flag(cancel)
        .with_mode(params.mode)
        .with_timeout_secs(resolve_deadline_secs(params.deadline_secs));
    if let Some(ctx) = project_context {
        runner = runner.with_project_context(ctx);
    }
    if let Some(catalog) = skills_catalog {
        runner = runner.with_skills_catalog(catalog);
    }
    apply_engineer_model_override(Arc::new(runner), params)
}

/// Discover the (cheap, metadata-only) skill catalog for `params.project` and
/// build a resolver over it — but only in `HarnessMode::DailyDriver`.
///
/// Why: #2069's scope note is explicit — "Parity mode should NOT
/// progressively disclose" — so `Parity` runs must never discover
/// `.claude/skills/` at all, let alone advertise the `use_skill` tool or
/// inject the catalog into the prompt.
/// What: Returns `None` for `HarnessMode::Parity` or when the project has no
/// (or an empty) skill catalog; otherwise `Some((rendered_catalog,
/// resolver))` — the resolver backs the `use_skill` tool registration.
/// Test: `task::executor::tests::daily_driver_skills_catalog_*`.
fn daily_driver_skills_catalog(params: &TaskRunParams) -> Option<(String, Arc<dyn SkillResolver>)> {
    if params.mode != HarnessMode::DailyDriver {
        return None;
    }
    let skills_dir = locate_skills_dir(&params.project);
    let resolver: Arc<dyn SkillResolver> = Arc::new(FsSkillResolver::new(skills_dir));
    let catalog = format_skill_catalog(&resolver.metadata());
    if catalog.is_empty() {
        None
    } else {
        Some((catalog, resolver))
    }
}

/// Builds the engineer's project-scoped tool registry for each delegation
/// (mirrors `run_task::ProjectToolFactory`, which is private to that module).
struct ProjectToolFactory {
    project: PathBuf,
    /// #2073: the delegating run's resolved `HarnessMode`, threaded onto the
    /// engineer's `EditTool` so `HarnessMode::Parity` selects edit-format
    /// order the same way regardless of which model is delegated to (§5.9's
    /// edit-format reconciliation). `run_task::ProjectToolFactory` (the
    /// legacy CLI path, which never resolves a `HarnessMode`) intentionally
    /// does not carry this field — its `EditTool` stays on the pre-#2073
    /// plain per-model order, unchanged.
    mode: HarnessMode,
}

#[async_trait]
impl RegistryFactory for ProjectToolFactory {
    async fn build(&self, agent: &AgentConfig, ctx: &RunContext) -> Arc<ToolRegistry> {
        // #2068: resolve the engineer's model slug so `EditTool` can pick its
        // per-model edit-format fallback order (mirrors
        // `run_task::ProjectToolFactory::build`, which is private to that module).
        let model_slug = resolve_model(agent, Some(ctx));
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(ReadFileTool::new(&self.project)));
        reg.register(Arc::new(WriteFileTool::new(&self.project)));
        reg.register(Arc::new(
            EditTool::new(&self.project)
                .with_model_slug(model_slug)
                .with_mode(self.mode),
        ));
        reg.register(Arc::new(BashTool::new(
            Some(self.project.clone()),
            Duration::from_secs(ENGINEER_BASH_TIMEOUT_SECS),
        )));
        reg.register(Arc::new(FinishTaskTool::new()));
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
/// model.
///
/// Why: `InProcessAgentRunner` resolves the engineer's model internally and
/// does not expose it; `run_task::resolve_agent_model_slug` (#2061's #1475
/// bug 1 fix) is the ONE shared implementation of "load config, apply
/// override else resolve from config, degrade to `unknown` on load
/// failure" this daemon path and `run_task::execute_run_task`'s legacy CLI
/// path both now call, so they can never independently drift.
/// What: thin wrapper binding this module's `ENGINEER_AGENT_NAME` constant
/// and `params.model_override`.
fn resolve_engineer_model(params: &TaskRunParams) -> String {
    resolve_agent_model_slug(
        &params.agents_dir,
        ENGINEER_AGENT_NAME,
        params.model_override.as_deref(),
    )
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
